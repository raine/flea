# flea

Use your coding agent to find and sell secondhand items on **Tori.fi** and
**Vinted**. Flea connects tools like Claude Code, Codex, and OpenCode to these
marketplaces, so you can describe what you want instead of navigating forms.

Ask your agent to:

- Find listings that match your budget, size, or location
- Prepare a listing from your photos and item details
- Review a draft with you, then publish it
- Update your existing listings
- Save favorites and set up search alerts on Tori

For example:

> Find dining chairs on Tori in Helsinki, Espoo, or Vantaa for under €100.

> Search Vinted for Marimekko dresses under €80.

> Help me sell this coat on Vinted. The photos are in ~/Pictures/coat.
> Ask me for any missing details and show me the draft before publishing.

## Install

With Homebrew on macOS or Linux:

```sh
brew install raine/flea/flea
```

Or use the installer:

```sh
curl -fsSL https://raw.githubusercontent.com/raine/flea/main/scripts/install | bash
```

<details>
<summary>Cargo and Nix</summary>

```sh
cargo install --git https://github.com/raine/flea --locked
nix profile install github:raine/flea
```

</details>

## Update

For installer or Cargo installations:

```sh
flea update
```

This installs the latest GitHub release for macOS or Linux (amd64 or arm64),
verifies its SHA-256 checksum, and atomically replaces the running executable.
It requires `tar` and write access to the executable directory; it does not
invoke sudo. Symlinks are resolved to the actual executable. Cargo and local
source builds are replaced with the published release, even if their version
is newer. An identical version is left unchanged.

Homebrew users should run `brew upgrade raine/flea/flea`; Nix users should update
through their Nix profile or configuration. Flea refuses to overwrite those
managed installations. No background update checks are performed.

Progress goes to stderr, with a plain success message by default. Use
`flea update --format json` for a structured result. Authentication, browser
profiles, and installed agent skills are unchanged; run `flea skill install`
after updating to refresh the bundled skill.

## Get started

### 1. Connect your coding agent

Install the bundled skill, which teaches your agent how to use Flea:

```sh
flea skill install
```

This detects Claude Code, Codex, and OpenCode. To choose one explicitly, use
`--agent claude`, `--agent codex`, or `--agent opencode`.

### 2. Sign in

**Tori:** Searching and viewing public listings work without an account. Sign in
when you want to sell, save favorites, or manage alerts:

```sh
flea tori auth login
```

**Vinted:** Sign in before searching or selling:

```sh
flea vinted auth login
```

For Vinted selling, connect Flea to your normal Google Chrome on macOS or Linux:

```sh
flea extension setup
```

The command installs bundled extension files and registers Flea's native bridge.
Open `chrome://extensions`, enable **Developer mode**, choose **Load unpacked**,
and select the extension directory printed by setup. Open or reload one
`https://www.vinted.fi` tab and sign in normally.

Flea automatically uses the extension after setup. No separate browser profile,
debugging port, or repeated connection approval is required. Keep exactly one
Vinted tab open in the Chrome profile with the extension installed. Check the
connection with:

```sh
flea vinted auth status --browser
```

The extension handles browser publication and listing edits. Vinted's catalog
credentials are still configured through `flea vinted auth login`. Sign out of
the shared browser session on the Vinted website; Flea does not clear your normal
Chrome cookies. Run setup again after moving the Flea executable, or to install
updated extension files, then reload the extension and Vinted tab.

Without extension setup, Flea retains its automatically managed, separate Chrome
profile (also supporting Chromium on Linux). Explicit `--browser-url` overrides
the extension if you prefer CDP.

To use an existing Chrome through CDP instead, enable debugging in
`chrome://inspect/#remote-debugging`, then run:

```sh
flea --browser-url http://localhost:9222 vinted auth status --browser
```

On macOS and Linux, Flea automatically keeps that approved connection alive in a
background helper. Keep passing the same URL on subsequent commands; no separate
server command or terminal is needed. To release access without closing Chrome:

```sh
flea browser disconnect --browser-url http://localhost:9222
```

### 3. Ask your agent

Tell your agent what you'd like to find or sell. Include useful details such as
your budget, location, item condition, and photo paths. For selling, ask it to
show you the listing before publishing.

You still handle account verification and any human checks required by the
marketplace.

## Prefer the terminal?

You can also use Flea directly:

```sh
flea tori search "tuoli" --area Helsinki,Espoo,Vantaa
flea vinted search "Marimekko" --price-to 80 --sort newest
```

Output is structured for agents and scripts rather than a traditional terminal
UI. Add `--format json` if you need JSON:

```sh
flea --format json tori search "tuoli"
```

Use `--help` on any command for options and examples:

```sh
flea --help
flea tori search --help
flea vinted draft --help
```

The detailed marketplace guides are bundled with the installed version:

```sh
flea skill tori
flea skill vinted
```

## Your photos and account

- Flea removes embedded photo metadata, including GPS location, before upload.
  Your original files are left untouched.
- JPEG and PNG are supported, along with HEIC/HEIF through macOS's built-in
  converter or the optional `heif-convert` tool on other platforms.
- Credentials and Vinted's dedicated browser profile are stored locally.
  Sign out with `flea tori auth logout` or `flea vinted auth logout`. When using
  the extension, sign out of Vinted directly in Chrome.
- Keep Flea's browser debugging connection local. Never expose it to the
  network. Prefer the extension for access to your everyday browser session.
- The extension is scoped to Vinted Finland. It exposes defined marketplace
  requests, not arbitrary JavaScript, and keeps browser credentials in Chrome.

## Development

Run `just check` to validate changes and `cargo run -- --help` to try the
local build. Extension tests require Node.js 22 or newer. Rust tests use [cargo-nextest](https://nexte.st/), included in the Nix
development shell or installable with `cargo install cargo-nextest --locked`.

## License

[MIT](LICENSE). Flea is an independent, unofficial tool, not affiliated with
or endorsed by Tori.fi or Vinted.
