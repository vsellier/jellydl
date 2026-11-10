# jellydl

A terminal app to browse and search a Jellyfin library and download videos.

- Browse libraries and folders, and search videos.
- Sort library lists like the Jellyfin web client, and jump page by page.
- See the director, writers and cast of the selected video.
- See the poster of the selected item. Terminals with an image protocol (kitty, Ghostty, WezTerm,
  iTerm2, Konsole, foot, Windows Terminal...) show the image. Other terminals show a preview
  made of colored characters. Turn it off with `posters = false` or `--no-posters`.
  In tmux 3.4+ built with sixel support, jellydl lets tmux draw the poster, so it stays correct
  when you change pane or window. Other tmux versions need `allow-passthrough` (jellydl turns it on
  for its pane), and the poster can stay on screen or disappear when you change pane.
- Download one video, several marked videos, or all the videos in a folder or library.
- Run several downloads at the same time with a limit you can change (`+` / `-`).
- Pause, resume, cancel and retry. Downloads resume from the partial file (HTTP Range).
- The queue is saved. Downloads that were running when you quit resume at the next start.
- Proxy support: HTTP, HTTPS, SOCKS5 and SOCKS5h, with or without a username and password.
- Login with a username and password, or with an API key.

## Legal and responsible use

jellydl is not an official Jellyfin project. It is not made, approved or supported by the Jellyfin team.
"Jellyfin" is the name of the Jellyfin project.

jellydl uses the public Jellyfin API with a normal user account.
It does not remove DRM and does not go around any access control.
The server only lets you download when your account has the **Allow media downloading** permission.

You are responsible for how you use jellydl:

- Use it only on a server that you own, or where the administrator lets you download.
- Download only content that you have the right to copy. The rules for private copies are different in each country.
- Do not use the `user_agent`, `[identity]` or start delay options to hide your downloads from the server administrator.
  These options are for compatibility (for example, a proxy or firewall that blocks unknown clients)
  and to keep the load on small servers low.

jellydl is provided "as is", without warranty.

## Install

```sh
cargo install --path .
```

## Configuration

At the first start, jellydl writes an example to `~/.config/jellydl/config.toml`. Edit it:

```toml
server_url   = "https://jellyfin.example.org"
download_dir = "~/Videos/jellyfin"
max_parallel = 3

# Optional. If not set, HTTP_PROXY / HTTPS_PROXY / ALL_PROXY are used.
proxy    = "socks5h://user:pass@127.0.0.1:1080"
no_proxy = "localhost,192.168.0.0/16"

# Optional. Default: "jellydl/<version>". Change it only when a proxy or firewall
# blocks unknown clients.
user_agent = "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0"

[auth]
username = "me"
password = "secret"          # or: password_cmd = "pass show jellyfin"
# api_key = "..."            # instead of username/password
```

Notes:

- Use `socks5h://` to resolve host names through the proxy. With `socks5://`, DNS is resolved locally.
- The user needs the **Allow media downloading** permission in Jellyfin.
- An API key has no user. jellydl then browses as the first admin user.

Command-line flags override the file:

```sh
jellydl --proxy socks5h://127.0.0.1:1080 --download-dir /mnt/nas/media -j 4
jellydl --check      # log in, list the libraries and exit: tests auth and proxy
jellydl --no-posters # do not show posters
```

## Docker

The image contains only the jellydl binary: no shell and no other programs.
jellydl can read and write only the folders you give it:

| Folder in the container | Content |
|---|---|
| `/config` | `jellydl/config.toml` |
| `/data` | `jellydl/`: download queue, log, session, device id |
| `/downloads` | downloaded files |

Your SSH keys, GitHub token and other files stay out of reach of jellydl and its libraries.

### Get the image

Use the published image (Linux amd64, arm64 and 32-bit ARM v7):

```sh
docker pull ghcr.io/vsellier/jellydl:latest
```

Or build it from the source:

```sh
docker build -t jellydl .
```

The examples below use `jellydl` as the image name. With the published image, use
`ghcr.io/vsellier/jellydl:latest`.

### First start

Make the folders, then start jellydl once. It writes an example config and stops:

```sh
mkdir -p ~/jellydl/config ~/jellydl/data ~/Videos/jellyfin
docker run --rm --user "$(id -u):$(id -g)" \
  -v ~/jellydl/config:/config \
  -v ~/jellydl/data:/data \
  -v ~/Videos/jellyfin:/downloads \
  jellydl
```

Edit `~/jellydl/config/jellydl/config.toml`, and keep it private:

```sh
chmod 600 ~/jellydl/config/jellydl/config.toml
```

In the container:

- `download_dir` in the config is not used. Files always go to `/downloads`, the folder you mount there.
- `password_cmd` does not work, because the image has no shell. Use `password`, or an API key.

### Run

```sh
docker run --rm -it --user "$(id -u):$(id -g)" \
  -e TERM -e TERM_PROGRAM \
  -v ~/jellydl/config:/config:ro \
  -v ~/jellydl/data:/data \
  -v ~/Videos/jellyfin:/downloads \
  jellydl
```

- `-it` is necessary: jellydl is a terminal app.
- `--user` makes the downloaded files belong to you.
- `-e TERM -e TERM_PROGRAM` lets jellydl find the image support of your terminal, for the posters.
- `:ro` mounts the config read-only. jellydl writes to the config folder only at the first start.
- Command-line flags go after the image name, for example `jellydl --check` or `jellydl --no-posters`.

If the Jellyfin server or the proxy is on the same machine (`localhost`), add `--network host`.
In the container, `localhost` is the container itself.

In tmux, the container cannot set tmux options. To see the posters, turn on passthrough
once with `tmux set -g allow-passthrough on`, or use `--no-posters`.

To type less, add an alias to your shell config:

```sh
alias jellydl='docker run --rm -it --user "$(id -u):$(id -g)" -e TERM -e TERM_PROGRAM -v ~/jellydl/config:/config:ro -v ~/jellydl/data:/data -v ~/Videos/jellyfin:/downloads ghcr.io/vsellier/jellydl:latest'
```

### Check the image

Each published image has a signed build provenance attestation, like the archives:

```sh
gh attestation verify oci://ghcr.io/vsellier/jellydl:<version> --repo vsellier/jellydl
```

## Files on disk

```text
<download_dir>/<Name>.<Year>/<Name>.<Year>.<ext>
<download_dir>/<Name>/SNN/<Name>.SNNEMM.<Title>.<ext>
<download_dir>/<Name>.<ext>
```

For example: `Elodie.2001/Elodie.2001.mkv`, `Garden.Diaries/S01/Garden.Diaries.S01E03.Pilot.mkv` and `Holiday.2025.mkv`.

- Names contain only letters, digits, dashes and dots. Spaces and special characters become dots.
- Accented letters become plain letters (`é` becomes `e`). Letters with no plain version, such as `東京`, are kept.

Jellyfin and Plex read the title, year and number (`S01E03`) from these names, so you can add the folder to a media server.

Older versions of jellydl put the files in a folder by type, first with spaces in the names
(`Elodie (2001)`), then with dots (`Elodie.2001`).
jellydl still finds these files: it shows them as downloaded and does not download them again.
A download in progress is written to `<file>.part`, then renamed.

The queue, log and device id are in `~/.local/share/jellydl/`
(`queue.json`, `jellydl.log`, `device_id`). Set `JELLYDL_LOG=debug` for more logs.

## Keys

| Key | Action |
|---|---|
| `Tab` / `1` `2` `3` | Library, Search, Downloads tab |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn`, `g` `G` | move |
| `Enter` / `→` | open the selected item |
| `Esc` / `←` / `Backspace` | go back |
| `/` | search. In the search box: `←` `→`, `Home` `End`, `Ctrl+A` `Ctrl+E` move the cursor, `Del` deletes the next character |
| `d` | download the selected video, or the marked videos. On a folder or library: all the videos inside |
| `Space` / `a` | mark one item / mark all |
| `r` | reload the list |
| `[` `]` / `{` `}` | previous / next page, first / last page (100 items per page) |
| `s` | sort menu: name, date added, release date, rating, runtime; ascending or descending |
| `o` | reverse the sort order |
| `?` | help |
| `q` | quit (asks first if downloads are active; the queue is saved) |

Downloads tab:

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k`, `PgUp` `PgDn`, `g` `G` | move |
| `p` / `Space` | pause or resume |
| `x` / `Del` | cancel a running download. On a queued, paused, failed or canceled one: remove it from the list. Both delete the partial file |
| `r` | retry a failed or canceled download |
| `P` / `R` | pause all / resume all |
| `c` | clear done and canceled downloads |
| `+` / `-` | more or fewer parallel downloads |

## Development

```sh
cargo test       # unit tests + download tests against a fake HTTP server
cargo clippy --all-targets -- -D warnings
cargo audit      # dependencies with known vulnerabilities (cargo install cargo-audit)
```

The `Audit` GitHub workflow runs `cargo audit` when the dependencies change, and every Monday.
A release also stops if a dependency has a known vulnerability.

## Release

Push a version tag. The version must match `Cargo.toml`:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The `Release` GitHub workflow runs the tests, builds the binaries, creates a GitHub release with them,
and publishes the Docker image `ghcr.io/vsellier/jellydl` (tags `<version>`, `<major>.<minor>` and `latest`):

| Archive | System |
|---|---|
| `jellydl-<version>-linux-amd64.tar.gz` | Linux x86_64 (static, any distribution) |
| `jellydl-<version>-linux-arm64.tar.gz` | Linux ARM64 (static, any distribution) |
| `jellydl-<version>-linux-armv7.tar.gz` | Linux 32-bit ARM, armv7 with hardware float (static, any distribution) |
| `jellydl-<version>-macos-amd64.tar.gz` | macOS Intel |
| `jellydl-<version>-macos-arm64.tar.gz` | macOS Apple Silicon |

To build the binaries and the image without publishing them, run the workflow by hand from the Actions tab.

The first time the image is published, GitHub makes the package private.
To let everyone pull it, open the package settings on GitHub and set its visibility to public.

### Check a downloaded binary

Each archive has a signed build provenance attestation. It proves that the archive
was built by the `Release` workflow of this repository, from the commit of the tag,
and that nobody changed it after. Check it with the [GitHub CLI](https://cli.github.com/):

```sh
gh attestation verify jellydl-<version>-linux-amd64.tar.gz --repo vsellier/jellydl
```

The command fails if the archive was changed, or was built somewhere else.
The `.sha256` file only checks that the download is complete: anyone who can
change the archive can also change that file.

```sh
sha256sum -c jellydl-<version>-linux-amd64.tar.gz.sha256
```

## License

MIT. See [LICENSE](LICENSE).
