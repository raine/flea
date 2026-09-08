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

## Get started

### 1. Connect your agent

```sh
flea skill install
```

Flea detects Claude Code, Codex, and OpenCode and installs the skill your agent
needs to use it.

### 2. Sign in

For Tori, you can search without an account. Sign in to sell, save favorites,
or manage alerts:

```sh
flea tori auth login
```

For Vinted, sign in to search or sell:

```sh
flea vinted auth login
```

To sell on Vinted, also [connect the browser extension](#sell-on-vinted).

### 3. Ask your agent

> Find dining chairs on Tori near Helsinki for under €100.

> Help me sell this coat on Vinted. The photos are in ~/Pictures/coat.
> Show me the listing before publishing.

Your agent handles the details. You handle account verification and any human
checks required by the marketplace.

## Sell on Vinted

Selling on Vinted requires Flea's extension in Google Chrome on macOS or Linux.
It lets your agent publish and edit listings through your signed-in browser,
without exporting your browser credentials. Searching does not need it.

1. Run `flea extension setup`.
2. Open `chrome://extensions` and enable **Developer mode**.
3. Choose **Load unpacked** and select the directory printed by setup.
4. Open `https://www.vinted.fi` and sign in. Keep exactly one Vinted tab open
   in that Chrome profile.

Check the connection with `flea vinted auth status --browser`.

<details>
<summary>Setup tips and troubleshooting</summary>

- On macOS, interactive setup copies the extension path to your clipboard.
  In the folder picker, press **Command+Shift+G**, paste, and press Return.
- If Vinted was already open, reload the tab after installing the extension.
- Run `flea vinted auth login` for search access, even if you are signed in
  through Chrome.
- After moving the Flea executable or updating the extension, run
  `flea extension setup` again, then reload the extension and Vinted tab.
- To sign out of the browser session, use Vinted's website. Flea does not clear
  your Chrome cookies.

</details>

## Update

- **Homebrew:** `brew upgrade raine/flea/flea`
- **Installer or Cargo:** `flea update`
- **Nix:** update through your profile or configuration.

Then run `flea skill install` to refresh your agent's skill.

## Prefer the terminal?

You can also use Flea directly:

```sh
flea tori search "tuoli" --area Helsinki,Espoo,Vantaa
flea vinted search "Marimekko" --price-to 80 --sort newest
flea --format json vinted search jacket --seller-country FI --max-shipping 3 --limit 10
```

Output is structured for agents and scripts. Add `--format json` for JSON.
Use `--help` on any command for options, or read the bundled marketplace guides
with `flea skill tori` and `flea skill vinted`.

## Your photos and account

- Flea removes embedded photo metadata, including GPS location, before upload.
  Your original files are left untouched.
- JPEG and PNG are supported, along with HEIC/HEIF through macOS's built-in
  converter or the optional `heif-convert` tool on other platforms.
- Catalog credentials are stored locally. Sign out with `flea tori auth logout`
  or `flea vinted auth logout --api`. To sign out of Vinted's shared browser session,
  use Vinted's website in Chrome. Flea does not clear normal Chrome cookies.
- The extension is scoped to Vinted Finland. It exposes defined marketplace
  requests, not arbitrary JavaScript, and keeps browser credentials in Chrome.

## Development

Run `just check` to validate changes and `cargo run -- --help` to try the
local build. Extension tests require Node.js 22 or newer. Rust tests use
[cargo-nextest](https://nexte.st/), included in the Nix development shell or
installable with `cargo install cargo-nextest --locked`.

## License

[MIT](LICENSE). Flea is an independent, unofficial tool, not affiliated with
or endorsed by Tori.fi or Vinted.
