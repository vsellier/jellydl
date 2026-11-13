# jellydl

A terminal app to browse and search a Jellyfin library and download videos.

![jellydl: a library of family videos sorted by name, with the image and details of the selected video](docs/screenshot.png)

<sub>Screenshot made with a demo server.</sub>

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
- A web interface (`--serve`) for a NAS or a server: browse, search, download and follow the queue from a browser.

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

### From a release binary

Each [GitHub release](https://github.com/vsellier/jellydl/releases) has an archive for each system:

| Archive | System |
|---|---|
| `jellydl-<version>-linux-amd64.tar.gz` | Linux x86_64 (static, any distribution) |
| `jellydl-<version>-linux-arm64.tar.gz` | Linux ARM64 (static, any distribution) |
| `jellydl-<version>-linux-armv7.tar.gz` | Linux 32-bit ARM, armv7 with hardware float (static, any distribution) |
| `jellydl-<version>-macos-amd64.tar.gz` | macOS Intel |
| `jellydl-<version>-macos-arm64.tar.gz` | macOS Apple Silicon |

Download the archive for your system. For example, version 0.1.0 for Linux x86_64:

```sh
version=0.1.0
archive=jellydl-$version-linux-amd64.tar.gz
curl -LO https://github.com/vsellier/jellydl/releases/download/v$version/$archive
```

[Check the archive](#check-a-downloaded-binary), then extract it and copy the binary to a folder in your `PATH`:

```sh
tar -xzf $archive
install -m 755 jellydl-$version-linux-amd64/jellydl ~/.local/bin/jellydl
jellydl --version
```

On macOS, the binary is not signed by Apple. If you download the archive with a web browser,
macOS blocks the binary. Remove the block after you check the archive:

```sh
xattr -d com.apple.quarantine ~/.local/bin/jellydl
```

`curl` does not add this block.

### Check a downloaded binary

Each archive has a signed build provenance attestation. It proves that the archive
was built by the `Release` workflow of this repository, from the commit of the tag,
and that nobody changed it after. Check it with the [GitHub CLI](https://cli.github.com/)
(run `gh auth login` first):

```sh
gh attestation verify $archive --repo vsellier/jellydl
```

For a stricter check, also require the `Release` workflow and the tag of the version:

```sh
gh attestation verify $archive --repo vsellier/jellydl \
  --signer-workflow vsellier/jellydl/.github/workflows/release.yml \
  --source-ref refs/tags/v$version
```

The command shows `Verification succeeded!` and the workflow that built the archive.
It fails if the archive was changed, or was built somewhere else. Do not use the binary then.

### Check that the file is the release asset

The releases are immutable: after a release is published, nobody can change, add or remove its archives,
and GitHub signs a release attestation with the tag, the commit and the SHA-256 digest of each archive.
The release page also shows the SHA-256 digest of each archive.

Check that your file is the archive that the `Release` workflow uploaded to the release:

```sh
gh release verify v$version --repo vsellier/jellydl
gh release verify-asset v$version $archive --repo vsellier/jellydl
```

The first command checks the release attestation. The second command fails if your file is
not an asset of this release, for example if it was changed after the download.

Use both checks: `gh attestation verify` proves who built the archive and from which commit,
and `gh release verify-asset` proves that the file is the one published in the release.

### Check the download with the `.sha256` file

The `.sha256` file only checks that the download is complete: anyone who can
change the archive can also change that file.

```sh
curl -LO https://github.com/vsellier/jellydl/releases/download/v$version/$archive.sha256
sha256sum -c $archive.sha256     # on macOS: shasum -a 256 -c $archive.sha256
```

### From the source

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

### Web interface on a NAS

Run jellydl as a service, with the web interface:

```sh
docker run -d --name jellydl --restart unless-stopped --user "$(id -u):$(id -g)" \
  -e JELLYDL_LISTEN=0.0.0.0:8080 -p 127.0.0.1:8080:8080 \
  -v ~/jellydl/config:/config:ro \
  -v ~/jellydl/data:/data \
  -v /volume1/video:/downloads \
  jellydl --serve
```

- `JELLYDL_LISTEN=0.0.0.0:8080` makes jellydl listen on all addresses in the container.
  `-p 127.0.0.1:8080:8080` publishes the port only on the NAS itself: a reverse proxy on the NAS
  can reach it, other machines cannot.
- `docker stop jellydl` saves the queue. Downloads continue at the next start.
- With folders by type, use paths in the container: for example
  `movies_dir = "/downloads/Films"` and `series_ended_dir = "/downloads/Séries terminées"`.
  To use other disks, mount them too (`-v /volume2/series:/series`) and use these paths.

See [Web interface](#web-interface) about the login.

### Check the image

Each published image has a signed build provenance attestation, like the archives:

```sh
gh attestation verify oci://ghcr.io/vsellier/jellydl:<version> --repo vsellier/jellydl
```

## Web interface

```sh
jellydl --serve                         # http://127.0.0.1:8080/
jellydl --serve --listen 0.0.0.0:8080   # or JELLYDL_LISTEN=0.0.0.0:8080
```

The web interface does the same things as the terminal interface:

- The first screen shows `New movies`, `New 4K movies`, `New series`, then the libraries.
- Open a library, a series or a season. Sort the library lists. Search.
- Click a video to see its details, its cast, and where it is (or will be) on disk.
- Select videos (the box at the top left of a poster) and download them, or download a whole
  folder, series or season with `Download all`.
- The `Downloads` tab shows the queue live: pause, resume, cancel, retry, number of parallel downloads.

The terminal and the web interface use the same config, folders and queue. Run only one of them at a time.

**There is no login.** Anyone who can open the page can start downloads with your Jellyfin account.
By default jellydl listens only on `127.0.0.1`. To use it from other machines, put a reverse proxy
with authentication in front of it (for example Caddy or nginx with basic auth, or an SSO proxy).
The page uses server-sent events (`/api/events`): turn off response buffering for this path in the proxy
(nginx: `proxy_buffering off;`).

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

### Folders by type, and names from the server

By default everything goes to `download_dir` with the clean names above. You can split the downloads:

```toml
download_dir       = "~/Videos/jellyfin"          # everything that is not set below
movies_dir         = "~/Videos/Movies"
series_ongoing_dir = "~/Videos/Series"            # series that still get new episodes
series_ended_dir   = "~/Videos/Series (ended)"    # finished series
naming             = "server"                     # default: "clean"
```

With `naming = "server"`, jellydl keeps the file name that the server has:

```text
<movies_dir>/Wicked Partie II 2025.mkv
<series_dir>/Ted Lasso (2020)/S01/Ted.Lasso.S01E01.MULTi.VFi.WiTH.AD.1080p.WEBRip.AC3.5.1.H265-TARDiS.mkv
```

Only the characters that a file system does not accept are changed. The series folder is
`Name (Year)`, with the year that Jellyfin has for the series. If the server has no file name,
jellydl builds one from the title and the year.

A series goes to `series_ended_dir` when Jellyfin says its status is `Ended`, and to `series_ongoing_dir`
when it is `Continuing` or unknown. A series that already has a folder in one of the two
folders stays there, even if its status changed. Move the folder yourself when a series ends.
jellydl looks in all the folders to know if a file is already downloaded.

Older versions of jellydl put the files in a folder by type, first with spaces in the names
(`Elodie (2001)`), then with dots (`Elodie.2001`).
jellydl still finds these files: it shows them as downloaded and does not download them again.
A download in progress is written to `<file>.part`, then renamed.

The queue, log, device id, and UI state (sort and last check) are in `~/.local/share/jellydl/`
(`queue.json`, `jellydl.log`, `device_id`, `ui_state.json`). Set `JELLYDL_LOG=debug` for more logs.

## Keys

| Key | Action |
|---|---|
| `Tab` / `1` `2` `3` | Library, Search, Downloads tab |
| `↑` `↓` / `j` `k`, `PgUp` `PgDn`, `g` `G` | move |
| `Enter` / `→` | open the selected item |
| `Esc` / `←` / `Backspace` | go back |
| `/` | search. In the search box: readline keys (see below) |
| `d` | download the selected video, or the marked videos. On a folder or library: all the videos inside |
| `Space` / `a` | mark one item / mark all |
| `r` | reload the list |
| `[` `]` / `{` `}` | previous / next page, first / last page (100 items per page) |
| `s` | sort menu: name, date added, release date, rating, runtime; ascending or descending |
| `o` | reverse the sort order |
| `n` | show only the items added since the last check, or all items again. The list of libraries is not filtered |
| `N` | mark all as seen: set the last check to now (asks first) |
| `?` | help |
| `q` | quit (asks first if downloads are active; the queue is saved) |

The first screen starts with three entries, `New movies`, `New 4K movies` and `New series`.
Each one lists the 100 latest additions of the server, newest first (4K = a video stream of about 3840 pixels wide or more).

Text boxes (the search box) support the readline keys:

| Key | Action |
|---|---|
| `Ctrl+A` `Ctrl+E` / `Home` `End` | start / end of the line |
| `Ctrl+B` `Ctrl+F` / `←` `→` | one character left / right |
| `Alt+B` `Alt+F` / `Ctrl+←` `Ctrl+→` | one word left / right |
| `Ctrl+H` / `Backspace`, `Ctrl+D` / `Del` | delete the character before / after the cursor |
| `Ctrl+W` / `Alt+Backspace`, `Alt+D` | delete the word before / after the cursor |
| `Ctrl+U`, `Ctrl+K` | delete to the start / to the end of the line |
| `Ctrl+Y` | paste the text deleted by the last `Ctrl+W`, `Ctrl+U`, `Ctrl+K` or `Alt+D` |
| `Ctrl+T` | swap the two characters around the cursor |
| `Alt+U` `Alt+L` `Alt+C` | upper case, lower case, capitalize the next word |
| `Ctrl+N`, `Enter`, `Esc` | leave the box |

A movie or episode is new when it was added to the server after the last check.
A series, season or folder is new when it holds a new file.
New items have a `new` tag in the lists.
The last check moves only when you press `N`.
At the first start, it is set to the start time, so nothing is new yet.

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

Before the first release, turn on immutable releases: on GitHub, open **Settings** > **General** >
**Releases** and select **Enable release immutability**. Without it, `gh release verify-asset` does not work.

Push a version tag. The version must match `Cargo.toml`:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

The `Release` GitHub workflow runs the tests, builds the binaries, creates a GitHub release with them,
and publishes the Docker image `ghcr.io/vsellier/jellydl` (tags `<version>`, `<major>.<minor>` and `latest`).
The archives are listed in [Install](#from-a-release-binary).

To build the binaries and the image without publishing them, run the workflow by hand from the Actions tab.

The first time the image is published, GitHub makes the package private.
To let everyone pull it, open the package settings on GitHub and set its visibility to public.

## License

MIT. See [LICENSE](LICENSE).
