# Setting up, building and testing ER Note on the MacBook Air

A start-to-finish guide for getting this repository working on the M1 MacBook Air. Each
part builds on the one before it, so go in order the first time. Every step says what to
type, what you should see, and what to do if you see something else.

Written 2026-11-18. The platform-independent flows were run successfully on the Linux
machine that day:

- the local CLI;
- the API, worker and remote CLI together;
- migrations, onboarding and dev tokens;
- the CI application gate.

The Mac-specific steps (Homebrew, Xcode, the Simulator) haven't been run on the Mac yet.
When one behaves differently, update this guide.

---

## What you will have at the end

1. All the Mac tools installed: Xcode, Homebrew, Docker, Rust, Python, Terraform.
2. The Rust workspace building on the Mac.
3. The developer CLI running a full mock patient visit in **local mode** (no server).
4. The local backend running: Postgres, an S3 stand-in, an SQS stand-in, the API and the worker.
5. The CLI running in **remote mode** against that local backend, including server-side MDM.
6. The full Rust test suite passing.
7. The iOS app building and running in the iPhone Simulator.

Plan on **3–5 hours** the first time. Most of it is waiting for downloads (Xcode is large)
and for the first Rust build.

---

## Status as of 2026-11-18: read this first

These problems were found while writing this guide and fixed in the Linux working copy.
**The fixes reach the Mac only after you commit and push them (Part 2).**

| Problem | Fix |
|---|---|
| MinIO deleted its public Docker images in 2026-09, so the S3 stand-in couldn't be pulled. | Both compose files now use **RustFS** 1.0.6, an Apache-2.0 S3-compatible server with arm64 builds. It passes the same versioned-upload and Terraform-lock tests. |
| The iOS bridge build failed in its symbol check: `nm: … Unknown attribute kind (105)`. Rust 1.97.1 is built on a newer LLVM than Xcode 26.6. | `scripts/apple_bridge.py` now passes `--no-llvm-bc` to Xcode's `nm`. The failure was reproduced with an older `nm` and the flag fixes it, but it hasn't run on a Mac or in CI yet. |
| `ANTHROPIC_KEY=` (blank, as `.env.example` ships it) made the CLI quit at startup. | The CLI now treats a blank key as "no key" and uses the mock AI, as the worker already did. |
| CI scripts called `rg` (ripgrep), which GitHub's Ubuntu runners don't have. | The scripts use standard `grep` now. |

Still open:

- **Dependency audit.** `scripts/ci-audit.py` blocks on RUSTSEC-2026-0459, a bug in the
  SQLite bundled by `libsqlite3-sys` 0.30.1. The patched 0.39 can't be selected yet,
  because every released `sqlx` version pins the old one in the lockfile. A decision is
  pending.
- **Advisory exception.** The existing `rsa` exception in `ci/security-exceptions.json`
  expires on 2026-11-20.

---

## How the pieces fit

The Rust workspace in `crates/` is the whole product: domain model, local storage, AI
calls, server, worker and phone core. On the Mac you use it three ways:

- **Local mode.** `er-cli` with a SQLite file in the current folder. Needs nothing else.
- **Remote mode.** `er-cli` talking to `er-server` and `er-worker` running on the Mac.
  Those need Postgres, RustFS (the S3 stand-in) and ElasticMQ (the SQS stand-in), all in
  Docker.
- **iOS.** The Rust phone core (`er-device`/`er-ffi`) is compiled into an XCFramework that
  the SwiftUI app in `ios/` links against. That needs Xcode.

```
┌─────────────────────────────── your Mac ───────────────────────────────────┐
│                                                                            │
│  er-cli ──HTTP──▶ er-server (API, :8080) ──┬──▶ Postgres  :5433  (Docker)  │
│                                            ├──▶ RustFS    :9000  (Docker)  │
│                                            └──▶ ElasticMQ :9324  (Docker)  │
│                                                     ▲                      │
│                         er-worker (mock AI + STT) ──┘ also uses Postgres   │
│                                                       and RustFS           │
│                                                                            │
│  Xcode ──▶ ErNote.app in the Simulator ──links──▶ RustCore.xcframework     │
└────────────────────────────────────────────────────────────────────────────┘
```

---

## Part 1 — Check the Mac (5 minutes)

Open **Terminal** (⌘-Space, type "Terminal", Enter). Run:

```bash
sw_vers                                   # macOS version
uname -m                                  # chip architecture
sysctl -n hw.memsize | awk '{print $1/1073741824 " GB RAM"}'
df -h ~ | tail -1                         # free disk space ("Avail" column)
```

What you need:

- **`uname -m` prints `arm64`.** That confirms Apple Silicon (M1). The iOS scripts refuse to run on anything else.
- **macOS Tahoe 26.2 or later.** `sw_vers` should show `ProductVersion: 26.2` or higher. Xcode 26.6, the version CI uses, requires it. If the Mac is older, update first: Apple menu → System Settings → General → Software Update. The M1 Air supports Tahoe.
- **At least 80 GB free.** Rough sizes:
  - Xcode plus iOS and watchOS simulators: about 25–35 GB.
  - Docker: about 5 GB.
  - Rust toolchains: about 3 GB.
  - Build output (`target/`): grows over time. On the Linux machine it reached 140 GB after months of work. Part 16 shows how to shrink it.
- **Note the RAM figure.** If it says 8 GB, you'll set two lower limits later (Parts 5 and 8).

---

## Part 2 — Put your newest work on GitHub (on the Linux machine)

The Mac gets the code from GitHub, so anything that is only on the Linux machine won't be
there. That currently includes the Phase 5 Step 3 work, the fixes listed above,
`docs/explainers/` and this guide. On the **Linux** machine:

```bash
cd ~/code-personal/projects/er-note-app
git status                 # review what is uncommitted
git add -A                 # or add files selectively
git commit -m "..."        # your message
git push origin master
```

`.env`, `dev/keys/` (except the committed fixture key) and the databases are git-ignored
on purpose. You recreate them on the Mac in Part 8; don't copy them over.

---

## Part 3 — Install Xcode (1–2 hours, mostly downloading)

Xcode supplies the compilers, the iOS SDKs and the Simulator. Rust also needs Xcode's
linker to build anything on a Mac, so install Xcode before Rust.

### 3.1 Download the same Xcode that CI uses

CI pins **Xcode 26.6, build 17F113** (`.github/workflows/ci.yml`). Using the same version
on the Mac means anything that works locally also works in CI, and the other way round.

1. In Safari, go to <https://developer.apple.com/download/all/> and sign in with your Apple ID. A free account is enough.
2. Search for **Xcode 26.6** and download the `.xip` file (several GB).
3. Double-click the `.xip` in Finder. Expanding it takes 10–20 minutes, and the progress bar sits still for long stretches. That's normal.
4. Rename the resulting `Xcode.app` to **`Xcode_26.6.app`** and drag it into **Applications**. The name matches CI and lets you install another Xcode later without overwriting this one.

> Alternative: the Mac App Store's Xcode works too, but it's the newest release
> (Xcode 27 or later). Its version won't match CI. If you use it, read its real version
> in step 3.3 and use those numbers wherever this guide says `26.6` / `17F113`.

### 3.2 Point the command-line tools at it and finish first-launch setup

```bash
sudo xcode-select --switch /Applications/Xcode_26.6.app/Contents/Developer
sudo xcodebuild -license accept
xcodebuild -runFirstLaunch
```

`sudo` asks for your Mac login password. Nothing appears while you type it.

### 3.3 Download the simulator platforms and confirm the version

```bash
xcodebuild -downloadPlatform iOS        # the iPhone simulator (several GB)
xcodebuild -downloadPlatform watchOS    # needed because the project has a watch app
xcodebuild -version
xcode-select -p
```

You should see:

```
Xcode 26.6
Build version 17F113
/Applications/Xcode_26.6.app/Contents/Developer
```

Write these two values down. They become `EXPECTED_XCODE_VERSION` and
`EXPECTED_XCODE_BUILD` in Part 14.

---

## Part 4 — Install Homebrew and the command-line tools (15 minutes)

Homebrew is the Mac's package manager. Everything below installs into `/opt/homebrew`.

### 4.1 Install Homebrew

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

When it finishes, it prints two "Next steps" commands. They are:

```bash
echo 'eval "$(/opt/homebrew/bin/brew shellenv)"' >> ~/.zprofile
eval "$(/opt/homebrew/bin/brew shellenv)"
```

Check with `brew --version`.

### 4.2 Install the tools this repository needs

```bash
brew install git gh python bash cmake gnu-sed tfenv
```

What each one is for, and why the version macOS ships with isn't enough:

| Tool | Why this project needs it |
|---|---|
| `git` | Newer than Apple's copy. |
| `gh` | Signs in to GitHub and clones the private repository (Part 7). |
| `python` | The bridge script `scripts/apple_bridge.py` needs **Python 3.11 or newer** (it uses `tomllib` and `hashlib.file_digest`). The `python3` that comes with macOS is 3.9 and fails. |
| `bash` | macOS ships bash **3.2** from 2007. Some scripts, for example `scripts/tf-network-check.sh`, expand an empty array under `set -u`, which bash 3.2 rejects as "unbound variable". Homebrew's bash 5 takes over because the scripts start with `#!/usr/bin/env bash`. Your login shell stays zsh. |
| `cmake` | Insurance for the `aws-lc-sys` crate, which compiles C code. Current versions don't need it on macOS, but some build paths do. |
| `gnu-sed` | The Terraform check scripts use Linux-style `sed -i`, which macOS's `sed` rejects. You only put it in front of the system `sed` for those scripts (Part 15). |
| `tfenv` | Installs the exact Terraform version pinned in `.terraform-version` (1.10.4). Homebrew's own `terraform` package is stuck at an old version. |

### 4.3 Open a new Terminal window and verify

Close Terminal completely (⌘Q) and reopen it so the new PATH takes effect. Then:

```bash
which python3 && python3 --version    # /opt/homebrew/bin/python3, Python 3.11 or newer
which bash && bash --version | head -1 # /opt/homebrew/bin/bash, version 5.x
```

If `which python3` prints `/usr/bin/python3`, Homebrew isn't first in your PATH. Run the
`eval "$(/opt/homebrew/bin/brew shellenv)"` line from 4.1 again and reopen Terminal.

---

## Part 5 — Install Docker Desktop (10 minutes)

Docker runs the three local services in containers:

- Postgres, the database;
- RustFS, the S3 stand-in;
- ElasticMQ, the SQS stand-in.

1. Download **Docker Desktop for Mac — Apple Silicon** from
   <https://www.docker.com/products/docker-desktop/>. Open the `.dmg` and drag Docker into
   Applications.
2. Open Docker from Applications and accept the terms. Wait for the whale icon in the menu
   bar to stop animating.
3. In Docker Desktop, open **Settings → Resources** and set **Memory**:
   - **2–3 GB** on an 8 GB Mac.
   - **4 GB** on a 16 GB Mac.

   The Rust builds run on the Mac itself, not inside Docker, so a small Docker limit
   leaves memory for compiling.
4. Verify:

```bash
docker version --format '{{.Server.Os}}/{{.Server.Arch}}'   # linux/arm64
docker compose version
docker run --rm hello-world                                 # prints "Hello from Docker!"
```

All three service images have native arm64 builds, so no x86 emulation is needed.

> Docker Desktop is free for personal use and small businesses. OrbStack
> (`brew install --cask orbstack`) is a lighter alternative that accepts the same `docker`
> and `docker compose` commands, if the Air feels sluggish.

---

## Part 6 — Install Rust (5 minutes)

Use the official installer, **not** `brew install rust`. The repository pins an exact
compiler version (`rust-toolchain.toml` → 1.97.1), and only `rustup` honors that pin.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Press **Enter** to accept the default installation. Then load it into the current
terminal, which new terminals do automatically:

```bash
. "$HOME/.cargo/env"
rustup --version
cargo --version
```

You'll install the pinned 1.97.1 compiler from inside the repository in Part 7.

---

## Part 7 — Get the code (5 minutes)

### 7.1 Sign in to GitHub

```bash
gh auth login
```

Answer the prompts:

| Prompt | Answer |
|---|---|
| Where do you use GitHub? | **GitHub.com** |
| Preferred protocol | **SSH** |
| Generate a new SSH key? | **Yes** |
| Passphrase | Your choice |
| Title for the key | Something like "MacBook Air" |
| How to authenticate | **Login with a web browser** |

`gh` then shows a one-time code and opens github.com for you to paste it. This also
uploads the new SSH key to your GitHub account.

### 7.2 Clone

```bash
mkdir -p ~/code
cd ~/code
gh repo clone zaugust4891/er-clinician-app
cd er-clinician-app
git log --oneline -3          # the top commit should be the one you pushed in Part 2
grep -n 'image:' docker-compose.yml   # should list rustfs/rustfs, not minio/minio
```

The rest of this guide assumes the repository is at **`~/code/er-clinician-app`**. If you
put it somewhere else, change that path in the helper file in Part 8.3.

### 7.3 Install the pinned Rust compiler and its extras

Run this from **inside** the repository folder:

```bash
rustup toolchain install 1.97.1 --profile minimal \
  -c rustfmt -c clippy \
  -t aarch64-apple-ios -t aarch64-apple-ios-sim
rustup show active-toolchain
rustup target list --installed --toolchain 1.97.1
```

Expected:

```
1.97.1-aarch64-apple-darwin (overridden by '/Users/<you>/code/er-clinician-app/rust-toolchain.toml')
aarch64-apple-darwin
aarch64-apple-ios
aarch64-apple-ios-sim
```

What these are:

- **`rustfmt` and `clippy`**: the formatter and linter. CI fails on any format or lint problem.
- **The two `-ios` targets**: let Rust compile the phone core for a real iPhone and for the Simulator.
- No watchOS Rust target is needed; the watch app contains no Rust.

---

## Part 8 — One-time project setup (5 minutes)

### 8.1 Create `.env`, the development settings file

`.env` is git-ignored. Create it from the checked-in template, then make one edit:

```bash
cp .env.example .env
sed -i '' 's/^BIND_ADDR=.*/BIND_ADDR=127.0.0.1:8080/' .env
grep -n '^BIND_ADDR' .env              # BIND_ADDR=127.0.0.1:8080
```

(`sed -i ''` with the empty quotes is the macOS form of "edit the file in place".)

That edit keeps the dev API reachable from this Mac only, not from the Wi-Fi network. It
also avoids a macOS firewall prompt.

Leave `ANTHROPIC_KEY=` and `DEEPGRAM_API_KEY=` blank. Blank means the CLI and the worker
use the built-in **mock** AI and transcriber: no network, no cost, predictable output. To
try real Claude later, put a real key after `ANTHROPIC_KEY=`, and use synthetic data only.

### 8.2 Create the public half of the dev signing key

The repository includes a **public, fake** private key, `dev/keys/dev_private.pem`, used
only to sign local test logins. Derive its public key:

```bash
openssl pkey -in dev/keys/dev_private.pem -pubout -out dev/keys/dev_public.pem
head -1 dev/keys/dev_public.pem        # -----BEGIN PUBLIC KEY-----
```

You'll generate the third file, `jwks.json`, after the first build (Part 9.2). Never use
these keys for anything real: they're published in the repository.

### 8.3 Create a helper file that sets up any terminal for this project

Every terminal you use for ER Note work needs the same handful of settings. Put them in
one file **outside** the repository:

```bash
cat > ~/.ernote-env <<'EOF'
# Load in every ER Note terminal with:   source ~/.ernote-env
cd ~/code/er-clinician-app || return
set -a; . ./.env; set +a      # dev database, S3/SQS stand-ins, dev login settings
export APP_ENV=dev            # servers refuse to start without it (.env can't set it)
export SQLX_OFFLINE=true      # compile database queries from the checked-in .sqlx cache
# Keep these dev terminals away from any real AWS account or Bedrock:
export AWS_EC2_METADATA_DISABLED=true AWS_CONFIG_FILE=/dev/null AWS_SHARED_CREDENTIALS_FILE=/dev/null
export AWS_ENDPOINT_URL_BEDROCK_RUNTIME=http://127.0.0.1:9
# Smaller and faster builds, the same as CI (delete this line if you need a debugger):
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
# On an 8 GB Mac, uncomment to avoid running out of memory while linking:
# export CARGO_BUILD_JOBS=4
EOF
source ~/.ernote-env
echo "APP_ENV=$APP_ENV SQLX_OFFLINE=$SQLX_OFFLINE DATABASE_URL=$DATABASE_URL"
```

Notes:

- **Always run `source ~/.ernote-env` before building.** Cargo treats the
  `CARGO_PROFILE_*` settings as part of the build. If you build once with them and once
  without, it recompiles everything.
- **`SQLX_OFFLINE=true` matters.** The server's SQL queries are checked at compile time.
  Offline mode checks them against the cached files in `.sqlx/`, so building never needs
  a running database.
- **These terminals are deliberately "AWS-blind"** (`AWS_CONFIG_FILE=/dev/null`). Use a
  separate terminal, without this file, for real AWS SSO work.

---

## Part 9 — Build (10–30 minutes the first time)

### 9.1 Build the whole workspace

```bash
source ~/.ernote-env
cargo build --locked --workspace
```

`--locked` means "use exactly the versions in `Cargo.lock`", the same as CI. The first
build downloads and compiles a few hundred crates; the AWS SDK crates are the slowest.
The fanless Air may get warm and slow down. That's normal.

Finished looks like:

```
    Finished `dev` profile [unoptimized] target(s) in 14m 12s
```

### 9.2 Generate the dev login key set (`jwks.json`)

The API checks login tokens against this file:

```bash
cargo run -q --locked -p er-server --bin mint_dev_token -- jwks > dev/keys/jwks.json
grep -o '"kid":"[^"]*"' dev/keys/jwks.json         # "kid":"dev-key-1"
```

### 9.3 Format and lint check (what CI runs first)

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Both print nothing on success except clippy's `Finished` line. `cargo fmt --all`
(without `--check`) fixes formatting automatically.

---

## Part 10 — Try it: local mode (5 minutes, no services needed)

Local mode is the developer CLI with a SQLite file in the current folder. No Docker, no
server.

```bash
source ~/.ernote-env
cargo run -q --locked --bin er-cli
```

The banner should include:

```
Mode: LOCAL
Database: ./er_notes.db
Status: Fresh shift
```

Type these commands one at a time at the `>` prompt:

```
new chest pain
switch 1
start ambient
stop
lab troponin 0.01 ng/mL
med given aspirin 324mg PO
dx Chest pain --primary R07.9
dispo discharge Return-if-worse Cardiology-1-week
mdm
note
quit
```

What you should see, abbreviated, from the run on 2026-11-18:

```
> Registered patient 01a1…c5 with chief complaint 'chest pain'
> Active patient: chest pain (d9b19ec5)
> Started Ambient encounter a78f1fed.
> Stopped encounter a78f1fed.
> Added lab: troponin = 0.01 ng/mL
> Recorded medication [given]: aspirin 324mg PO
> Diagnosis assigned: Chest pain (primary) [R07.9]
> Disposition set: Discharge (instructions: Return-if-worse Cardiology-1-week)
> MDM generated and note moved to Finalizing.
[mock MDM: moderate complexity, 2 diagnoses considered]
> ED PROVIDER NOTE
  …
  HPI:  [mock HPI from encounter transcript]
  Medical Decision Making:  [mock MDM: moderate complexity, 2 diagnoses considered]
```

Things to know:

- **Patient numbers start at 1.** `switch 0` is rejected; use `list` to see the numbers.
- **The bracketed `[mock …]` text** is the mock AI. It proves the pipeline runs end to end.
- **Run it again and it resumes the shift.** The banner says "Resumed shift". That's the
  crash-recovery design: events are saved to `./er_notes.db` and the shift to
  `./shift.json`. Both are git-ignored. Delete them to start fresh:
  `rm -f er_notes.db* shift.json`.
- **`help`** lists every command.

---

## Part 11 — Start the local services (5 minutes)

The server, the worker and most tests need three services, all defined in
`docker-compose.yml`.

### 11.1 Start them

```bash
source ~/.ernote-env
docker compose up -d
docker compose ps          # postgres, s3 and elasticmq should all show "Up"
```

The first start downloads three images, about 600 MB in total.

### 11.2 Confirm all three answer

```bash
docker compose exec -T postgres pg_isready -U postgres -d ernote
curl -fsS http://127.0.0.1:9000/health/ready >/dev/null && echo "RustFS ready"
curl -fsS -d 'Action=ListQueues&Version=2012-11-05' http://127.0.0.1:9324 >/dev/null && echo "ElasticMQ ready"
```

Expected:

```
/var/run/postgresql:5432 - accepting connections
RustFS ready
ElasticMQ ready
```

Ports:

- **Postgres: 5433**, not the usual 5432, so it won't collide with any other Postgres on the Mac.
- **RustFS web console:** <http://localhost:9001/rustfs/console/>, login `dev` / `devdevdev`. Use it to see uploaded audio.

### 11.3 Create the database tables

```bash
RUST_LOG=info cargo run -q --locked -p er-server --bin er-migrate
```

Look for these lines in the JSON log output:

```
"message":"migration", … "status":"complete"
"message":"grants","status":"skipped_without_runtime_roles"
```

The second line is normal in dev: the separate cloud database roles don't exist locally.
Re-running `er-migrate` is safe; it only applies migrations that haven't run yet.

---

## Part 12 — Run the tests (15–40 minutes the first time)

### 12.1 The full suite against your dev services

With the services from Part 11 running:

```bash
source ~/.ernote-env
cargo test --workspace --locked
```

This is the same test command CI runs. The server tests create their own throwaway
databases, buckets and queues, so they don't disturb your dev data.

What "passed" looks like: many blocks ending in lines like

```
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.12s
```

The output has no `FAILED` lines, and `echo $?` right after the command prints `0`.

Smaller runs for day-to-day work, which are much faster:

```bash
cargo test -p er-domain --locked                     # pure domain rules (no services needed)
cargo test -p er-app --locked                        # orchestration, crash recovery (no services)
cargo test -p er-server --test sync_gate --locked    # one server test file (needs services)
cargo test -p er-server --locked -- full_remote      # tests whose name contains "full_remote"
```

The tests leave throwaway queues behind in ElasticMQ, named `stt-jobs-test-…`. To clear
them: `docker compose restart elasticmq`. ElasticMQ keeps everything in memory.

### 12.2 CI's all-in-one application gate

`scripts/ci-services.sh` is exactly what CI's *Application* job runs. It starts its own
fresh copies of the three services on random ports, so your dev services and data are
never touched. It then runs:

- the format and lint checks;
- the OpenAPI check;
- the full test suite;
- the SQLx cache check;
- the missing-service checks.

It needs the SQLx command-line tool once:

```bash
cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features rustls,postgres
```

Then:

```bash
scripts/ci-services.sh
```

It ends with `Application, SQLx freshness and missing-service gates passed`. It
deliberately ignores `~/.ernote-env` and `.env`, so you don't need to source anything
first.

---

## Part 13 — Run the whole system: API, worker and remote CLI (15 minutes)

This is the "cloud" path, all on the Mac: the CLI talks to the API over HTTP, and the
worker generates the MDM in the background. Keep the Docker services from Part 11
running. Open two more Terminal tabs (⌘T).

### 13.1 Tab "API"

```bash
source ~/.ernote-env
RUST_LOG=info cargo run -q --locked -p er-server --bin er-server
```

From another tab, check it: `curl -s http://127.0.0.1:8080/healthz` → `{"status":"ok"}`.

### 13.2 Tab "Worker"

```bash
source ~/.ernote-env
RUST_LOG=info cargo run -q --locked -p er-server --bin er-worker
```

Expect two loud warnings and a ready line:

```
"DEEPGRAM_API_KEY is not set — every chunk will receive a CANNED transcript …"
"TRANSPORT=direct but ANTHROPIC_KEY is not set — generated note prose will be CANNED …"
"transcription worker ready" …
```

The warnings are correct: the worker uses mocks because no provider keys are set.

### 13.3 Create a test clinician (once per fresh database)

The API only accepts a login whose user ID is registered as a clinician in an
organization. In the cloud, that ID comes from Cognito and `er-onboard` registers it.
Locally, you register a made-up ID yourself. In a spare tab:

```bash
source ~/.ernote-env
ONBOARDING_ACTION=apply \
ONBOARDING_REQUEST='[{"org_id":"7d2f5c1e-1111-4a2b-9c3d-000000000001","org_name":"Synthetic Test ED","cognito_sub":"11111111-2222-4333-8444-555555555555","display_name":"Synthetic Doctor"}]' \
cargo run -q --locked -p er-server --bin er-onboard
echo "exit code: $?"          # 0 means it worked
```

`er-onboard` enforces two rules, and breaking either one fails it with a vague
"unclassified" error:

- `cognito_sub` must be a **lowercase UUID**. On macOS, `uuidgen | tr A-Z a-z` makes one.
- `org_name` and `display_name` must both start with **`Synthetic `** in dev and staging.

### 13.4 Tab "CLI": use the app over HTTP

```bash
source ~/.ernote-env
export ER_SERVER_URL=http://127.0.0.1:8080
export ER_TOKEN=$(scripts/dev-token.sh 11111111-2222-4333-8444-555555555555)
cargo run -q --locked --bin er-cli
```

The token is a signed dev login valid for **24 hours**. After that, run the
`export ER_TOKEN=…` line again.

The banner now says:

```
Mode: REMOTE — http://127.0.0.1:8080
Storage: server (no local database, no shift file)
Status: Fresh shift
```

Type:

```
new chest pain
switch 1
start ambient
stop
lab troponin 0.01 ng/mL
dx Chest pain --primary
dispo discharge Return-if-worse
mdm
quit
```

The interesting part is `mdm`, which goes through the job queue to the worker:

```
> MDM queued (0789628b). Generating…
MDM job 0789628b status: queued
MDM job 0789628b status: done
MDM generation is done; the note was reloaded from the event stream.
MDM generated and note moved to Finalizing.
[mock MDM: moderate complexity, 2 diagnoses considered]
```

### 13.5 Optional: the "second device" demo

This shows that the server, not the device, holds the shift. Open another tab and start
the CLI from an empty folder with the same token:

```bash
source ~/.ernote-env
export ER_SERVER_URL=http://127.0.0.1:8080
export ER_TOKEN=$(scripts/dev-token.sh 11111111-2222-4333-8444-555555555555)
mkdir -p /tmp/device-b && cd /tmp/device-b
~/code/er-clinician-app/target/debug/er-cli
```

The banner says **Resumed shift** and lists the patient you created. Run `note` after
`switch 1` to see it. The folder stays empty: remote mode writes no local files.

### 13.6 Stop everything

Press **Ctrl-C** in the API and Worker tabs. Then:

```bash
docker compose stop                 # keeps the dev data for next time
```

---

## Part 14 — The iOS app: Xcode and Simulator (30–60 minutes)

> This part hasn't been completed on any Mac yet. CI's *Bridge Apple* job failed every
> run until the `nm` fix of 2026-11-18, which hasn't been through CI. If a step fails,
> the error message is the next thing to fix. Note it down.

### 14.1 Create the Apple settings file

The Apple scripts insist that you state exactly which Xcode and which simulator to use,
so a silent Xcode update can't change the results.

List the available iPhone simulators:

```bash
xcrun simctl list devices available | grep iPhone
```

Each line looks like `iPhone 17 (5C1A2B3D-1111-2222-3333-444455556666) (Shutdown)`.
Copy the long ID in parentheses from the phone you want, then:

```bash
cat > ~/.ernote-apple-env <<'EOF'
# Load after ~/.ernote-env for iOS work:   source ~/.ernote-apple-env
export DEVELOPER_DIR=/Applications/Xcode_26.6.app/Contents/Developer
export EXPECTED_XCODE_VERSION=26.6      # must match `xcodebuild -version` exactly
export EXPECTED_XCODE_BUILD=17F113
export IOS_SIMULATOR_UDID=PASTE-THE-SIMULATOR-ID-HERE
EOF
open -e ~/.ernote-apple-env               # opens TextEdit: paste the simulator ID, save
```

If you installed a different Xcode in Part 3, put its version and build here instead.

### 14.2 Load both files

```bash
source ~/.ernote-env
source ~/.ernote-apple-env
xcodebuild -version                       # must print exactly the two EXPECTED_ values
```

### 14.3 Host-side bridge checks (no Simulator involved)

```bash
scripts/check-bridge-linux.sh
```

Despite the "linux" in the name, nothing in this script is Linux-specific. It's what CI's
passing *Bridge Linux* job runs, and it should also work on the Mac, though it hasn't been
run there yet. It does four things:

- runs the phone-core tests;
- regenerates the Swift bindings and confirms they match the committed ones;
- checks that the phone build pulls in no server or AWS code;
- runs the bridge script's own tests.

It should end without errors.

### 14.4 Build the Rust XCFramework the app links against

```bash
scripts/build-xcframework.sh --check
```

This creates `ios/ErNoteKit/Artifacts/RustCore.xcframework` (git-ignored). Along the way
it runs a safety check that lists the library's exported symbols with `nm`, to prove no
test-only function shipped.

**If you see `nm: error: … Unknown attribute kind`:** your checkout predates the fix. Run
`git pull`. The fix makes the script pass `--no-llvm-bc` so that Xcode's `nm` reads the
real symbol table instead of the embedded LLVM bitcode. Rust 1.97.1 writes that bitcode
with a newer LLVM than Xcode 26.6 understands.

### 14.5 Run the app in the Simulator

```bash
open ios/ErNote.xcodeproj
```

In Xcode:

1. In the toolbar's scheme menu (left of the device name), choose **ErNote**.
2. In the device menu next to it, choose the iPhone simulator you picked in 14.1.
3. Press **⌘R** (Product → Run). The Simulator opens and launches **ER Note**, using the
   real Rust local core with the synthetic development scope.

If Xcode says it can't find `Artifacts/RustCore.xcframework`, step 14.4 didn't finish.

If Xcode says **"Signing for ErNote requires a development team"**: choose your personal
team in a git-ignored override file, then reopen the project.

```bash
echo 'DEVELOPMENT_TEAM = YOURTEAMID' > ios/Config/Local.xcconfig
```

Find your Team ID in Xcode → Settings → Accounts after adding your Apple ID. Simulator
builds normally don't need a team; the project has no entitlements that require one.

To run the in-app test suite from Xcode: choose the **ErNoteSmoke** scheme and press **⌘U**.

### 14.6 The scripted Apple checks (what CI's Bridge Apple job runs)

```bash
bash scripts/check-apple-shell.sh     # probes, XCFramework, Debug+Release simulator builds, app-hosted tests
scripts/check-bridge-apple.sh         # Debug+Release smoke tests + the isolated panic-handling test
```

They write only to git-ignored folders: `target/apple-*`, `ios/DerivedData` and
`ios/ErNoteKit/Artifacts`.

### 14.7 Record the toolchain

`docs/ops/ios-development.md` has a table of Mac facts that "must be recorded from the
M1". Fill it in with:

```bash
sw_vers
xcodebuild -version
xcodebuild -showsdks | grep -E 'iOS|watchOS'
```

Running on a **physical iPhone** needs an Apple Developer team, a registered bundle ID
(`ER_NOTE_BUNDLE_ID = …` in `ios/Config/Local.xcconfig`) and device registration. That's
outside this guide; see `docs/ops/ios-development.md`.

---

## Part 15 — Optional: the rest of CI's checks

None of these are needed to build or run the app. They reproduce the remaining CI jobs
locally. Run every command from the repository folder: start each session with
`source ~/.ernote-env`, which also changes into that folder.

### 15.1 SQLx query cache is up to date

This needs the migrated dev database from Part 11.3 and `sqlx-cli` from Part 12.2:

```bash
SQLX_OFFLINE=false cargo sqlx prepare --workspace --check -- --all-targets --locked
```

If it reports drift after you changed a SQL query, run the same command without
`--check` to refresh `.sqlx/`, then commit the result.

### 15.2 The published API description is up to date

```bash
cargo run -q --locked -p er-api --example openapi --features schema | cmp - docs/api/openapi.json \
  && echo "OpenAPI snapshot is current"
```

### 15.3 Python policy tests

```bash
python3 scripts/test_step10.py
python3 scripts/test_ci_policy.py
python3 scripts/test_deploy.py
python3 scripts/test_apple_bridge.py
```

### 15.4 Terraform (offline only: mocked AWS, no credentials, nothing created)

```bash
tfenv install          # reads .terraform-version → 1.10.4
tfenv use 1.10.4
terraform version      # Terraform v1.10.4 on darwin_arm64
PATH="$(brew --prefix gnu-sed)/libexec/gnubin:$PATH" scripts/ci-terraform.sh
```

The `PATH=…` prefix gives the script Linux-style `sed` for this command only. The script
also pulls a pinned `postgres:16` image for a log-privacy check, so Docker must be
running. Never run `terraform apply` from the Mac outside the staging runbook
(`docs/ops/staging-deploy.md`).

### 15.5 Secret scan and dependency advisories

```bash
brew install gitleaks actionlint
cargo install cargo-audit --version 0.22.2 --locked
actionlint .github/workflows/*.yml
python3 scripts/ci-security.py        # Homebrew's gitleaks is newer than CI's pinned 8.24.3
python3 scripts/ci-audit.py           # blocks on RUSTSEC-2026-0459 until that's decided (see the status section)
```

### 15.6 Not worth doing on the Air

`scripts/ci-image.sh` builds the **linux/amd64** server image. On Apple Silicon that runs
under emulation and takes a very long time. Leave it to CI.

---

## Part 16 — Everyday cheat sheet

**Start of a session:**

```bash
source ~/.ernote-env
docker compose up -d
```

**Typical loop:**

```bash
cargo build --locked --workspace
cargo fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test -p <crate> --locked                 # focused
cargo test --workspace --locked                # everything, before pushing
```

**End of a session:** `docker compose stop`.

**Reset the dev data:**

```bash
docker compose down -v                    # deletes the dev database and the dev S3 bucket
rm -f er_notes.db* shift.json             # deletes local-mode CLI state
```

After `down -v`, start the services again and redo Part 11.3 (migrate) and Part 13.3
(onboard).

**Free up disk space:**

```bash
du -sh target                             # how big the build output is
cargo clean                               # deletes target/ (the next build is a full rebuild)
docker system prune                       # deletes stopped containers and unused images
xcrun simctl delete unavailable           # deletes simulators for removed iOS versions
rm -rf ios/DerivedData                    # Xcode build output for this project
```

**After pulling new commits:**

```bash
git pull
source ~/.ernote-env
cargo build --locked --workspace
RUST_LOG=info cargo run -q --locked -p er-server --bin er-migrate   # if migrations changed
```

For iOS work, also rebuild the XCFramework (Part 14.4).

---

## Part 17 — Troubleshooting

| What you see | What it means | Fix |
|---|---|---|
| `pull access denied for minio/minio` | Your checkout predates the switch to RustFS. | `git pull` (see Part 2). |
| `Failed to create ClaudeSummarizer: … Anthropic API key must not be blank` | Your checkout predates the CLI fix. | `git pull`, or delete the `ANTHROPIC_KEY=` line from `.env`. |
| `ModuleNotFoundError: No module named 'tomllib'` or `module 'hashlib' has no attribute 'file_digest'` | The scripts are running on macOS's Python 3.9. | `which python3` must print `/opt/homebrew/bin/python3` (Part 4.3). |
| `…: unbound variable` from a `scripts/*.sh` file | The script is running on macOS's bash 3.2. | `which bash` must print `/opt/homebrew/bin/bash`. Reopen Terminal. |
| `sed: 1: "…": invalid command code` or `extra characters at the end of …` | A Linux-style `sed -i` ran on macOS's sed. | Use the `gnu-sed` `PATH=…` prefix (Part 15.4). |
| `xcrun: error: invalid active developer path` or `linker 'cc' not found` | The command-line tools don't point at Xcode. | `sudo xcode-select --switch /Applications/Xcode_26.6.app/Contents/Developer` |
| `Selected Xcode does not match the expected version/build` | `EXPECTED_XCODE_*` doesn't match the selected Xcode. | Compare `xcodebuild -version` with `~/.ernote-apple-env`. |
| `Missing Rust target: aarch64-apple-ios` or `Install aarch64-apple-ios and aarch64-apple-ios-sim…` | The iOS targets aren't installed for 1.97.1. | Rerun Part 7.3 from inside the repository. |
| `Remove Rust flag/wrapper/ios-release profile overrides for bridge builds` | `RUSTFLAGS`, `RUSTC_WRAPPER` (for example sccache) or `CARGO_PROFILE_IOS_RELEASE_*` is set. | `unset RUSTFLAGS RUSTC_WRAPPER` and remove it from your shell profile. |
| `nm: error: … Unknown attribute kind` | Your checkout predates the `nm` fix. | `git pull` (Part 14.4). |
| Xcode: missing `RustCore.xcframework` | The XCFramework hasn't been built. | Part 14.4. |
| `error communicating with database` **while compiling** | `SQLX_OFFLINE` isn't set, so SQLx tried to reach a database. | `source ~/.ernote-env` |
| `Connection refused` on ports 5433, 9000 or 9324 | A service isn't running. | `docker compose up -d`, then the readiness checks (Part 11.2). |
| `Bind for 0.0.0.0:9000 failed: port is already allocated` | Something else already uses port 9000. | `lsof -i :9000` to find it, then stop it. |
| API returns **401** | The login ID isn't onboarded, the token has expired (24 h), or the database was reset. | Part 13.3, then mint a new token (13.4). |
| `er-onboard` prints `startup failed … "unclassified"` | The ID isn't a lowercase UUID, or a name doesn't start with `Synthetic `. | Fix the request (Part 13.3). |
| `APP_ENV must be dev\|staging\|prod` or a similar startup error | `APP_ENV` isn't set in the shell. | `source ~/.ernote-env` |
| Builds suddenly recompile everything | You built once with and once without `~/.ernote-env`. | Always source it first. |
| The Mac grinds or freezes while linking tests | Not enough memory for parallel links. | Uncomment `CARGO_BUILD_JOBS=4` in `~/.ernote-env` and lower Docker's memory. |
| Disk full | `target/` has grown. | `cargo clean` (Part 16). |

---

## Where to read more

- `CLAUDE.md`: architecture summary and the history of each phase.
- `docs/ops/ci-verification.md`: what every CI gate checks.
- `docs/ops/ios-development.md`: Apple-side decisions, signing and device plans.
- `docs/ops/phase5-step2-implementation.md` and `docs/explainers/phase5-step2-device-core.html`: the phone core.
- `docs/explainers/aws-infrastructure.html` and `infra/README.md`: the AWS side. None of it is needed for local development.
