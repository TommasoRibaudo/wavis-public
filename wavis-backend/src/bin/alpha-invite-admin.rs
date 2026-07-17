use std::env;
use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;
use wavis_backend::auth::alpha_invite::{
    AlphaInviteAdminRecord, CreatedAlphaInvite, create_alpha_invite, disable_alpha_invite,
    list_alpha_invites,
};
use wavis_backend::redaction::Sensitive;

#[derive(Debug, Parser)]
#[command(
    name = "alpha-invite-admin",
    about = "Manage closed-alpha registration invites directly in Postgres"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Generate and persist a new invite, printing its raw code once.
    Create {
        #[arg(long)]
        label: String,
        #[arg(long, value_parser = parse_rfc3339)]
        expires_at: Option<DateTime<Utc>>,
        #[arg(long, default_value_t = 1)]
        max_redemptions: i32,
    },
    /// List invite metadata without exposing codes or hashes.
    List {
        #[arg(long)]
        label: Option<String>,
    },
    /// Disable an invite by UUID. Repeated calls preserve the first disable time.
    Disable {
        #[arg(long)]
        id: Uuid,
    },
}

fn parse_rfc3339(value: &str) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| timestamp.with_timezone(&Utc))
        .map_err(|_| "expected an RFC 3339 timestamp such as 2026-12-31T23:59:59Z".to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .try_init();

    let cli = Cli::parse();
    let stdout = io::stdout();
    let mut output = stdout.lock();

    match run(cli, &mut output).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "alpha invite admin command failed");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli, output: &mut impl Write) -> Result<()> {
    let database_url = Sensitive(
        env::var("DATABASE_URL").context("DATABASE_URL must be set for alpha invite operations")?,
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(database_url.inner())
        .await
        .context("failed to connect to Postgres")?;

    match cli.command {
        Command::Create {
            label,
            expires_at,
            max_redemptions,
        } => {
            let pepper = Sensitive(
                env::var("ALPHA_INVITE_CODE_PEPPER")
                    .context("ALPHA_INVITE_CODE_PEPPER must be set when creating an invite")?
                    .into_bytes(),
            );
            let created =
                create_alpha_invite(&pool, &label, expires_at, max_redemptions, &pepper).await?;
            write_created_invite(output, &created)?;
        }
        Command::List { label } => {
            let invites = list_alpha_invites(&pool, label.as_deref()).await?;
            write_invite_list(output, &invites, Utc::now())?;
        }
        Command::Disable { id } => {
            let invite = disable_alpha_invite(&pool, id).await?;
            write_disabled_invite(output, &invite)?;
        }
    }

    Ok(())
}

fn write_created_invite(output: &mut impl Write, created: &CreatedAlphaInvite) -> io::Result<()> {
    writeln!(output, "invite_id\t{}", created.invite.invite_id)?;
    writeln!(
        output,
        "label\t{}",
        display_label(created.invite.label.as_deref())
    )?;
    writeln!(output, "invite_code\t{}", created.raw_code.inner())?;
    writeln!(
        output,
        "expires_at\t{}",
        display_timestamp(created.invite.expires_at)
    )?;
    writeln!(
        output,
        "max_redemptions\t{}",
        created.invite.max_redemptions
    )
}

fn write_invite_list(
    output: &mut impl Write,
    invites: &[AlphaInviteAdminRecord],
    now: DateTime<Utc>,
) -> io::Result<()> {
    writeln!(
        output,
        "invite_id\tlabel\tstatus\tcreated_at\texpires_at\tdisabled_at\tredemptions\tmax_redemptions\tremaining"
    )?;
    for invite in invites {
        writeln!(
            output,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            invite.invite_id,
            display_label(invite.label.as_deref()),
            invite.status_at(now).as_str(),
            invite.created_at.to_rfc3339(),
            display_timestamp(invite.expires_at),
            display_timestamp(invite.disabled_at),
            invite.redemption_count,
            invite.max_redemptions,
            invite.remaining_redemptions(),
        )?;
    }
    Ok(())
}

fn write_disabled_invite(
    output: &mut impl Write,
    invite: &AlphaInviteAdminRecord,
) -> io::Result<()> {
    writeln!(output, "invite_id\t{}", invite.invite_id)?;
    writeln!(output, "status\t{}", invite.status_at(Utc::now()).as_str())?;
    writeln!(
        output,
        "disabled_at\t{}",
        display_timestamp(invite.disabled_at)
    )
}

fn display_label(label: Option<&str>) -> String {
    label
        .unwrap_or("(unlabeled)")
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

fn display_timestamp(timestamp: Option<DateTime<Utc>>) -> String {
    timestamp.map_or_else(|| "-".to_string(), |value| value.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wavis_backend::auth::alpha_invite::AlphaInviteStatus;

    fn record() -> AlphaInviteAdminRecord {
        AlphaInviteAdminRecord {
            invite_id: Uuid::nil(),
            label: Some("person@example.com".to_string()),
            created_at: DateTime::parse_from_rfc3339("2026-07-16T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            expires_at: None,
            disabled_at: None,
            max_redemptions: 1,
            redemption_count: 0,
        }
    }

    #[test]
    fn create_output_reveals_raw_code_exactly_once() {
        let raw_code = "ABCD-EF01-2345-6789-ABCD-EF01-2345-6789";
        let created = CreatedAlphaInvite {
            invite: record(),
            raw_code: Sensitive(raw_code.to_string()),
        };
        let mut output = Vec::new();

        write_created_invite(&mut output, &created).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches(raw_code).count(), 1);
        assert_eq!(format!("{:?}", created.raw_code), "[REDACTED]");
        assert_eq!(format!("{}", created.raw_code), "[REDACTED]");
        assert!(!format!("{created:?}").contains(raw_code));
    }

    #[test]
    fn list_and_disable_output_never_contain_secret_material() {
        let invite = record();
        let now = invite.created_at;
        let mut list_output = Vec::new();
        let mut disable_output = Vec::new();

        write_invite_list(&mut list_output, std::slice::from_ref(&invite), now).unwrap();
        write_disabled_invite(&mut disable_output, &invite).unwrap();

        let combined = format!(
            "{}{}",
            String::from_utf8(list_output).unwrap(),
            String::from_utf8(disable_output).unwrap()
        );
        assert!(!combined.contains("invite_code"));
        assert!(!combined.contains("code_hash"));
        assert!(!combined.contains("ABCD-EF01-2345-6789"));
        assert_eq!(invite.status_at(now), AlphaInviteStatus::Active);
    }

    #[test]
    fn legacy_control_characters_cannot_inject_output_rows() {
        assert_eq!(
            display_label(Some("legacy\tlabel\nvalue")),
            "legacy label value"
        );
    }
}
