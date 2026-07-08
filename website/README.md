# Wavis website

The marketing landing page for Wavis — a single static page built with Next.js and exported to plain HTML for hosting on S3 + CloudFront.

It matches the desktop app's look: the Catppuccin Mocha palette and JetBrains Mono, copied from `clients/wavis-gui/src/styles/theme.css`.

## Develop

```bash
cd website
npm install
npm run dev
```

Open `http://localhost:3000`.

## Build

```bash
npm run build
```

`next.config.ts` sets `output: "export"`, so the build writes a fully static site to `website/out/` (including `sitemap.xml`, `robots.txt`, and a `404.html`). Upload that directory to any static host.

## Downloads

The download buttons resolve real asset URLs at runtime from the GitHub Releases API (`/repos/TommasoRibaudo/wavis-public/releases/latest`), because release filenames embed the version (e.g. `Wavis_0.2.0_aarch64.dmg`) and would otherwise break each release. The visitor's OS is detected to highlight the matching platform and to point the hero button at it. If the API is unavailable, every button falls back to the releases page, and all links work with JavaScript disabled.

macOS builds are Apple Silicon only (`darwin-aarch64`), so the macOS option is labeled accordingly.

## Mailing list

The `/subscribe` form posts to a Google Form, which stores addresses in a linked Google Sheet. No backend or paid service required.

Set up:

1. Create a Google Form with a single short-answer question for the email address.
2. Link it to a Google Sheet: **Responses → Link to Sheets**.
3. Get the field id: open **⋮ → Get pre-filled link**, fill in a sample email, **Get link**, and copy it. The `entry.NNNNNNNNN` parameter is the email field id.
4. The submit endpoint is the form URL with `/viewform` replaced by `/formResponse`.
5. Put both values in `.env.local` (see `.env.example`):

```bash
NEXT_PUBLIC_SUBSCRIBE_FORM_ACTION=https://docs.google.com/forms/d/e/FORM_ID/formResponse
NEXT_PUBLIC_SUBSCRIBE_ENTRY_ID=entry.1234567890
```

Google Forms returns an opaque (`no-cors`) response, so the form validates the email locally and then shows an optimistic confirmation. Until the two variables are set, the form still validates and confirms but does not store anything (a warning is logged in development).

## Environment variables

| Variable | Purpose |
| --- | --- |
| `NEXT_PUBLIC_SITE_URL` | Canonical URL for metadata, sitemap, and robots. No trailing slash. |
| `NEXT_PUBLIC_SUBSCRIBE_FORM_ACTION` | Google Form `formResponse` endpoint. |
| `NEXT_PUBLIC_SUBSCRIBE_ENTRY_ID` | Google Form email field id (`entry.NNNNNNNNN`). |

## Deploy

`infrastructure/environments/dev/website.tf` provisions a private S3 bucket and a dedicated CloudFront distribution (separate from the API distribution). The `.github/workflows/deploy-website.yml` workflow builds the export and syncs it to S3, then invalidates CloudFront, on pushes to `main` that touch `website/**`.

## Replacing the app mock with a screenshot

`components/AppMock.tsx` renders a faithful mock of the Wavis window inside a fixed frame. To use a real screenshot, drop an `<img>` with explicit `width`/`height` into that frame — the surrounding layout will not shift.
