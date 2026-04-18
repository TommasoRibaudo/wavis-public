# macOS Compatibility Runner

Ships a built `Wavis.app` to macOS targets over SSH, runs tiered smoke tests,
and produces a merged diagnostic report.

---

## Machine model

Two independently toggleable Dedicated Hosts, one Intel and one ARM:

| Flag | Host type | macOS | Arch | Rate | 24 h min |
|---|---|---|---|---|---|
| `enable_mac_compat_intel=true` | mac1.metal | 13.x Ventura **or** 11.x Big Sur | x86_64 | ~$1.08/hr | ~$26 |
| `enable_mac_compat_arm=true` | mac2.metal | 26.x Tahoe | arm64 | ~$0.65/hr | ~$16 |

The Intel host runs **one macOS version at a time**, controlled by
`mac_intel_target_macos = "ventura" | "bigsur"`. Swapping between them
re-provisions the instance but keeps the Dedicated Host allocated — you pay
the 24-hour minimum **once** regardless of how many swaps you do that day.

You can enable just one host if that is all you need.

---

## Pricing

AWS bills Dedicated Hosts from allocation to release. There is a **24-hour
minimum billing period** per allocation. If you release a host and re-allocate
within 24 hours, you pay the minimum again.

**Do not release hosts between runs on the same day. Stop instances instead.**

| Configuration | Daily cost (worst case, 24 h) |
|---|---|
| Intel only | ~$26 |
| ARM only | ~$16 |
| Intel + ARM | ~$42 |
| Intel (Ventura) + swap to Big Sur same day | ~$26 (one host, no extra charge) |

---

## Recommended run order

**Start with the ARM host alone (~$16/day).** It is the cheapest option and
covers the baseline reference machine. If it passes, move to Intel. If it
fails, you have found a regression without spending more.

Once ARM passes, bring up the Intel host (~$26/day extra). Run Ventura first
— it supports the full tier suite including ScreenCaptureKit. If Ventura
passes, swap the same host to Big Sur at no extra charge and run the reduced
tier suite (t0/t1/t2) to verify SCK degrades gracefully on older macOS.

| Step | Host | macOS | Cost |
|---|---|---|---|
| 1 | ARM only | 26.x Tahoe | ~$16 |
| 2 | + Intel | 13.x Ventura | +~$26 |
| 3 | Intel (swap) | 11.x Big Sur | +$0 — same host |
| **Total** | | | **~$42** |

If you only have time or budget for one machine, use **ARM** (step 1). It
catches the most common regressions on the primary development target.

---

## Step-by-step

### 1. Check AZ availability

Mac instances are not available in every AZ. Find a supported one:

```sh
aws ec2 describe-instance-type-offerings \
  --location-type availability-zone \
  --filters "Name=instance-type,Values=mac1.metal" \
  --region us-east-2 --output table

# Repeat for mac2.metal if you need ARM
aws ec2 describe-instance-type-offerings \
  --location-type availability-zone \
  --filters "Name=instance-type,Values=mac2.metal" \
  --region us-east-2 --output table
```

### 2. Set Terraform variables

In `infrastructure/environments/dev/terraform.tfvars`:

```hcl
# Enable only what you need:
enable_mac_compat_intel = true    # ~$26/day
enable_mac_compat_arm   = false   # ~$16/day — add when needed

# Which macOS to load on the Intel host:
mac_intel_target_macos = "ventura"   # or "bigsur"

mac_compat_az = "us-east-2b"         # whichever AZ supports Mac instances

# If your existing subnet_id is not in mac_compat_az, supply one that is:
# mac_compat_subnet_id = "subnet-xxxxxxxxxxxxxxxxx"

# ARM AMI — if macOS 26 is not yet available, fall back to Sequoia:
# mac_arm_ami_name_filter = "amzn-ec2-macos-15.*"
```

To find available macOS AMIs:

```sh
aws ec2 describe-images \
  --owners amazon \
  --filters "Name=name,Values=amzn-ec2-macos-*" \
  --query 'sort_by(Images,&CreationDate)[-15:].{Name:Name,ID:ImageId,Arch:Architecture}' \
  --region us-east-2 --output table
```

### 3. Apply Terraform

```sh
terraform -chdir=infrastructure/environments/dev apply
```

Allow 10–15 minutes for first boot. Mac instances are slow to start.

### 4. Generate machines.local.toml

```sh
python tools/compat/gen-machines-local.py
```

This reads `terraform output -json` and writes `tools/compat/machines.local.toml`
with the live public IP(s), SSH user (`ec2-user`), and key path
(`infrastructure/wavis-backend-dev-jey.pem`). Only the hosts you enabled appear
in the file.

Add `--check-ssh` to immediately probe each host:

```sh
python tools/compat/gen-machines-local.py --check-ssh
```

The script also prints the exact stop/start/teardown commands with your real
instance IDs pre-filled.

### 5. Grant TCC permissions (once per instance)

Before Tier 2 or Tier 3 runs, grant microphone and screen-recording access on
each host. See [doc/testing/macos-compat.md](../../doc/testing/macos-compat.md).
For Tier 1 only, skip this step.

### 6. Build Wavis.app

Build from a macOS controller. For Tier 2/3 use a debug build (the
`__compat_check` Tauri command is debug-only):

```sh
cd clients/wavis-gui
npm run tauri build -- --debug   # Tier 2/3
# or
npm run build                    # Tier 0/1 only
```

### 7. Run the compat suite

```sh
# All enabled machines
python tools/compat/compat-run.py --app path/to/Wavis.app

# The single Intel machine by name
python tools/compat/compat-run.py --app path/to/Wavis.app --machine mac-ventura-intel

# Specific tiers only
python tools/compat/compat-run.py --app path/to/Wavis.app --tiers t0,t1

# Dry run
python tools/compat/compat-run.py --app path/to/Wavis.app --dry-run
```

### 8. After the run — stop, swap, or teardown

`gen-machines-local.py` prints the exact commands with your instance IDs
every time you run it. The three options:

---

**Running tests again within 24 hours — stop instances, keep hosts.**

```sh
aws ec2 stop-instances --region us-east-2 --instance-ids <intel-id> [<arm-id>]
```

Stopped instances do not charge for compute. The Dedicated Hosts continue at
their hourly rate, which you are already paying. Before the next run:

```sh
aws ec2 start-instances --region us-east-2 --instance-ids <intel-id> [<arm-id>]

# IPs change on start — refresh machines.local.toml
python tools/compat/gen-machines-local.py --check-ssh
```

---

**Switching macOS version on the Intel host (same day, no extra charge).**

The Intel host bills continuously. Swapping the AMI replaces the instance but
not the host — you stay within your existing 24-hour window.

```sh
# Change mac_intel_target_macos to "bigsur" (or "ventura") in terraform.tfvars, then:
terraform -chdir=infrastructure/environments/dev apply

# Refresh IPs and machine name in machines.local.toml
python tools/compat/gen-machines-local.py

# Run against the new macOS version
python tools/compat/compat-run.py --app path/to/Wavis.app --machine mac-bigsur-intel
```

Note: TCC grants do not carry over between AMIs — re-grant on the new instance.

---

**Done for the day — terminate instances, then release Dedicated Hosts.**

Instances must be terminated before hosts can be released.

```sh
aws ec2 terminate-instances --region us-east-2 --instance-ids <intel-id> [<arm-id>]

terraform -chdir=infrastructure/environments/dev destroy \
  -target=aws_ec2_host.compat_intel \
  -target=aws_ec2_host.compat_arm \
  -var="enable_mac_compat_intel=true" \
  -var="enable_mac_compat_arm=true"
```

Verify in the AWS Console (EC2 → Dedicated Hosts) that all hosts show
**Released**. A fresh allocation the next day starts a new 24-hour minimum.

---

## What each tier tests

| Tier | Where | Tests |
|---|---|---|
| **t0** | Local (no SSH) | `otool` deployment target, linked dylibs, `codesign` entitlements |
| **t1** | Remote | App launch, crash reports, system log — primary SCK safety check |
| **t2** | Remote | Tauri IPC bridge ping via `__compat_check` debug command |
| **t3** | Remote | SCK / audio-process-tap graceful-degradation probe, TCC state dump |

Big Sur (`mac-bigsur-intel`) runs t0/t1/t2 only — ScreenCaptureKit is absent
on macOS < 12.3 and t3 is skipped. Tier 1 verifies the app does not crash at
launch despite the missing framework.

---

## Reading the report

`compat-results/compat-report-<timestamp>.md` has one section per machine with
a pass/fail per tier and a failure note including the first relevant crash frame
or log line.

The structured JSON version is at `compat-results/compat-report-<timestamp>.json`.

---

## Physical machine fallback

Skip the Terraform steps entirely. Copy `machines.example.toml`, fill in LAN
addresses and your own SSH key, and continue from step 5. The runner is
identical.

---

## Files

```
tools/compat/
├── README.md                   ← you are here
├── compat-run.py               controller: orchestrates SSH, Tier 0, report
├── gen-machines-local.py       generates machines.local.toml from Terraform output
├── machines.example.toml       committed template
├── machines.local.toml         gitignored — your live inventory
├── agent/
│   └── run-agent.sh            uploaded to each target; runs tier checks
├── checks/
│   ├── t0-build.sh             local: otool + codesign validation
│   ├── t1-launch.sh            remote: launch, crash detection
│   ├── t2-ipc.sh               remote: IPC bridge ping
│   └── t3-media.sh             remote: SCK/audio probe + TCC dump
└── report/
    └── merge-report.py         merges per-machine JSON → Markdown summary
```

Terraform resources: `infrastructure/environments/dev/mac-compat.tf`

TCC runbook: `doc/testing/macos-compat.md`
