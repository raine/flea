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

Homebrew: `brew upgrade raine/flea/flea`. Nix: update through your profile or
configuration.

After updating, run `flea skill install` to refresh your agent's skill.

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

**Why does Vinted need an extension?** Flea publishes and edits listings through
Vinted's signed-in website. These requests use your browser session and the
page's security tokens, and Vinted may ask you to complete a human verification
check. The extension lets Flea make those requests from your normal Vinted tab
without exporting browser credentials, launching a separate Chrome profile, or
enabling a debugging port. Searching and browsing use separate API credentials
and do not need the extension.

For Vinted selling, connect Flea to your normal Google Chrome on macOS or Linux:

```sh
flea extension setup
```

The command installs bundled extension files and registers Flea's native bridge.
Open `chrome://extensions`, enable **Developer mode**, choose **Load unpacked**,
and select the extension directory printed by setup. On macOS, interactive setup
copies the path to your clipboard: press **Command+Shift+G** in the folder picker,
paste, and press Return. Explicit `--format json` or `--format toon` keeps
structured output without changing the clipboard. Open or reload one
`https://www.vinted.fi` tab and sign in normally.

Flea automatically uses the extension after setup. Keep exactly one Vinted tab
open in the Chrome profile with the extension installed. Check the connection
with:

```sh
flea vinted auth status --browser
```

The extension handles browser publication and listing edits. Vinted's catalog
credentials are still configured through `flea vinted auth login`. Sign out of
the shared browser session on the Vinted website; Flea does not clear your normal
Chrome cookies. Run setup again after moving the Flea executable, or to install
updated extension files, then reload the extension and Vinted tab.

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
flea --format json vinted search jacket --seller-country FI --max-shipping 3 --limit 10
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
- Catalog credentials are stored locally. Sign out with `flea tori auth logout`
  or `flea vinted auth logout --api`. To sign out of Vinted's shared browser session,
  use Vinted's website in Chrome. Flea does not clear normal Chrome cookies.
- The extension is scoped to Vinted Finland. It exposes defined marketplace
  requests, not arbitrary JavaScript, and keeps browser credentials in Chrome.

## Development

Run `just check` to validate changes and `cargo run -- --help` to try the
local build. Extension tests require Node.js 22 or newer. Rust tests use [cargo-nextest](https://nexte.st/), included in the Nix
development shell or installable with `cargo install cargo-nextest --locked`.

## License

[MIT](LICENSE). Flea is an independent, unofficial tool, not affiliated with
or endorsed by Tori.fi or Vinted.
