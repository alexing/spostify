# Spostify

Spostify is a personal fork of [Spotifast](https://github.com/crmne/spotifast),
a fast, native Spotify client written in Rust with
[egui](https://github.com/emilk/egui) that plays music through
[librespot](https://github.com/librespot-org/librespot). It tracks upstream
closely and adds a few custom features:

- **AI insights.** A sparkles button on albums, artists and songs opens a
  quick briefing written by Claude Sonnet 5.5 from
  [MusicBrainz](https://musicbrainz.org) credits. **Dig deeper** asks for a
  longer briefing that also searches the web.
- **Planned:** DJ tools (musical key, BPM and a waveform).

Everything else (library, search, Spotify Connect, themes, the Winamp mini
player, MilkDrop) works as in Spotifast; its
[guide](https://spotifast.rocks/) applies here too.

**Playback needs Spotify Premium.** Free accounts can browse and search, but
cannot play music.

## Build and run

Install [Rust](https://rustup.rs), then from this folder:

```sh
cargo run --release
```

Sign in with Spotify in your browser, then choose **Set up playback here**
in the device menu of the player bar. This computer appears as a Spotify
Connect device named **Spostify**.

To look at the interface without a Spotify account:

```sh
cargo run --features demo -- --demo
```

## Set up AI insights

AI insights use your own [Anthropic API key](https://console.anthropic.com/settings/keys).
Each new briefing is a paid request to Anthropic. Either:

- open **Settings > AI insights**, paste the key and choose **Save key**
  (it is kept in the system credential store, the macOS Keychain on a Mac), or
- set the `ANTHROPIC_API_KEY` environment variable before starting the app.

Briefings are cached, so reopening one costs nothing.

## Credits and license

Spostify is built on [Spotifast](https://github.com/crmne/spotifast) by
Carmine Paolino and its contributors; nearly all of the code is theirs.
Internally the app still uses Spotifast's names (crate, settings folder,
keychain entry), which keeps rebasing on upstream cheap.

It also uses [librespot](https://github.com/librespot-org/librespot),
[egui](https://github.com/emilk/egui), the [Inter](https://rsms.me/inter/)
typeface (OFL), and [Lucide](https://lucide.dev) icons (ISC).

This project is not affiliated with Spotify. Spotify is a trademark of
Spotify AB.

Licensed under the [MIT License](LICENSE), like Spotifast.
