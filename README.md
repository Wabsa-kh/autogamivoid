# autogamivoid

Scheduled automation that keeps [gamivoid.site](https://gamivoid.site) stocked
with PC game listings — the full catalogs of Steamrip and SteamUnlocked,
imported one by one, then kept up to date automatically.

## How it works

```
                  ┌──────────────────────────────────────────────┐
                  │  A-Z catalog hubs (the "down all games" list)│
                  │  steamrip.com/games-list-page/               │
                  │  steamunlocked.org/all-games/{letter}/       │
                  └──────────────┬───────────────────────────────┘
                                 │ scrape every game link
                                 ▼
                   match between sources (bucketed, fuzzy)
                                 │
                                 ▼
                   enrich from Steam storefront API
        (description, developer, release date, genres,
         Steam CDN cover + hero images, store link)
                                 │
                                 ▼
                   SEO content generator
        (meta title ≤65 chars, meta description ≤155 chars,
         meta keywords, rich HTML listing body with
         about/game details/install steps)
                                 │
                                 ▼
                   POST /api/admin/games on gamivoid
                   (one game at a time, action_limit per run,
                    state.json resumes where the last run stopped)
```

### Three run actions

| Action | What it does | When it runs |
|---|---|---|
| `catalog` | Walks the **A-Z hubs** (every letter page), imports **all** games one by one. Skips games already published & unchanged; resumes across runs via `state.json`. | Every 6 hours (`autogamivoid-catalog.yml`) — the first full import (~25k games) completes over consecutive runs |
| `updates` | Re-checks **only the games you already published** (from `state.json`) against the sources' fresh listings and PATCHes those whose download link or version changed. | Daily at 05:00 UTC (`autogamivoid-updates.yml`) |
| `sync` | Scrapes the sources' fresh listings and publishes new + changed games. A lighter complement to the catalog walk. | Weekly (`autogamivoid.yml`) |

All three share one reusable workflow (`.github/workflows/autogamivoid-run.yml`)
and the same write-safety model: **new games are created once, existing games
are only PATCHed when the chosen download actually changed, everything else is
skipped with zero API traffic.**

### The A-Z discovery method

The catalog run uses the sites' own "all games" indexes — the same lists a
human would use to browse everything:

- **Steamrip**: `https://steamrip.com/games-list-page/` — one giant page with
  every game (anchors like `<a href="/game-slug-free-download/">`), relative
  URLs joined to the base.
- **SteamUnlocked**: `https://steamunlocked.org/all-games/` — a hub of letter
  pages (`/all-games/a/` … `/all-games/z/`, plus `0-9`), each listing that
  letter's games via `game-link` anchors. Pagination inside a letter page is
  followed automatically (bounded at 60 pages per letter).

New games show up on these pages as the sites add them, so re-running
`catalog` picks them up; `state.json` makes each run cheap by skipping what's
already published and unchanged.

### SEO content

Every listing gets generated metadata tuned for "download" intent queries:

- **Meta title**: `{Game} Free Download PC (v{version})`, capped at 65 chars.
- **Meta description**: under 155 chars with the exact query, version, dev and
  a call to action.
- **Meta keywords**: title variants (`game free download`, `download game`,
  `game pc download`) plus genre/developer combinations, deduped, max 10.
- **Listing body**: structured HTML — lead paragraph, `About {Game}` (Steam's
  official blurb when available), a `Game Details` section (title, version,
  genre, developer, publisher, release date, platforms), and a `How to
  Download & Install {Game}` section — the kind of unique, structured content
  that ranks and gets rich results.

Fields are sent as `metaTitle` / `metaDescription` / `metaKeywords` alongside
the standard payload; if your site's API ignores unknown fields nothing breaks,
and wiring them into your `<head>` makes them effective.

### Published-games manifest

Every run exports `autogamivoid/published-games.json` — the list of everything
listed on your site (slug, title, version, source, publish date). It doubles as
a sitemap input and a simple audit log, and it's what the `updates` action
cross-references.

## Layout

```
autogamivoid/
├── Cargo.toml             # standalone crate in the workspace
├── config.example.json    # template config (dry_run: true by default)
└── src/
    ├── main.rs            # CLI: --action sync|catalog|updates, --dry-run
    ├── workflow.rs        # run_sync / run_catalog / run_updates
    ├── scraper.rs         # fresh listings + A-Z catalog walkers
    ├── seo.rs             # meta title/description/keywords + HTML body
    ├── matching.rs        # fuzzy matching incl. bucketed full-catalog mode
    ├── enrichment.rs      # Steam appdetails + CDN images
    ├── downloads.rs       # versioned download-choice logic
    ├── client.rs          # HTTP with bearer auth, retries, backoff
    ├── client_gamivoid.rs # gamivoid API (taxonomy/create/patch)
    ├── index.rs           # state.json + published-games.json export
    ├── models.rs          # serde types
    └── slug.rs            # stable slug derivation
```

## Setup

### 1. Secrets (GitHub repo → Settings → Secrets and variables → Actions)

| Secret | Value |
|---|---|
| `GAMIVOID_PUBLISHING_KEY` | Your `s4f_…` publishing key (never commit this) |
| `STEAMRIP_BASE_URL` | `https://steamrip.com` (optional; defaults to this) |
| `STEAMUNLOCKED_BASE_URL` | `https://steamunlocked.net` (redirects to .org) |
| `STEAM_WEB_API_KEY` | optional, unused by default enrichment |

### 2. First run — dry run

`config.example.json` ships with `"dry_run": true`:

```bash
cargo run -p autogamivoid -- autogamivoid/config.example.json --action catalog
```

Dry-run scrapes the whole A-Z catalog, matches, enriches and validates every
record but writes nothing. Force dry-run on any config with `--dry-run`.

### 3. Go live

- Copy `config.example.json` → `config.json`, set `dry_run: false` and your
  real key **locally only** — the workflows write the key from the secret.
- Trigger **Actions → Catalog import → Run workflow** and watch the first runs
  chew through the catalog. Tune `AUTOGAMIVOID_ACTION_LIMIT` (repo variable)
  to speed up or slow down the import.

### 4. Schedules

| Workflow | Cron | Purpose |
|---|---|---|
| `autogamivoid-catalog.yml` | `0 */6 * * *` | A-Z import, resumes each run |
| `autogamivoid-updates.yml` | `0 5 * * *` | Re-check published games for updates |
| `autogamivoid.yml` | `0 3 * * 0` | Fresh-listing sync (new + changed) |
| `autogamivoid-reconcile.yml` | manual | Heal state from the site, backfill incomplete drafts |
| `autogamivoid-reset.yml` | manual | DESTRUCTIVE: delete ALL listings, clear state, optionally relist from zero |

Each workflow commits updated `state.json` / `published-games.json` back to the
repo, which is what makes the next run resume and skip unchanged games.

### Publish modes

`publish_mode` in the config (or the workflow input) controls what happens when
a listing passes every publish gate:

- `draft`: every listing is created with `published: false`. Review
  in the dashboard, then flip them live there, or switch the mode.
- `publish` (**default** in the workflows and config.example.json): complete
  listings go live immediately; anything missing required data stays a draft.

### Direct download links (the real file-host URL)

After the A-Z crawl picks a game, the runner fetches that game's own page on
the source site and extracts what listing pages never show:

- **The actual download link** — SteamUnlocked's primary button
  (`su-dl-primary`, an uploadhaven.com URL) or Steamrip's file-host anchor
  (MegaDB, Gofile, ...). This is the first `downloadLinks` entry, so the
  site's download button leads straight to the file host instead of
  re-linking the source listing.
- **Real file size** ("28.11 GB") from the hero chip / "Game Size" meta /
  JSON-LD `fileSize`.
- **Version marker** ("v1.15 | Full Version") when the listing title has none.
- **Developer / publisher / genre** from the page's meta list.
- **System requirements** (OS/Processor/Memory/Graphics/Storage) so games
  Steam cannot resolve still get complete listing data.

**Images are Steam-only.** Cover, hero, featured and screenshot URLs are
always Steam CDN assets (`cdn.cloudflare.steamstatic.com`); nothing is ever
hotlinked from Steamrip or SteamUnlocked (the publishing guide forbids
hotlinking artwork without permission). A game whose Steam entry cannot be
resolved therefore has no images and stays a **draft** instead of publishing
with source-site artwork — run reconcile again later; it will publish once
Steam resolves.

### Listing conventions

- **Title & slug** carry the "Free Download" keyword: "Doom Free Download"
  with slug `doom-free-download` (matching the search intent of the site's
  audience and the source sites' own URL pattern).
- **Download links are file-host links only** — the exact URL behind the
  source page's download button (UploadHaven, MegaDB, ...). The Steam store
  page and the source listing pages never appear as links, and no label,
  note, article, license text or alt text ever mentions where the game was
  collected from.
- **Everything is filled.** Missing values get clean placeholders ("Latest"
  version, "10 GB available space" storage, a generic requirements block,
  "Not specified" rows in the details table), so no published page shows an
  empty field.
- **Requirements are formatted** one "Label: value" per line (OS / Processor
  / Memory / Graphics / DirectX / Storage), re-split from the glued run-on
  paragraphs the source pages use.

### The reconcile action

`--action reconcile` (or the manual workflow) is the recovery tool:

1. Lists every listing on the site (drafts included).
2. Rebuilds `state.json` from that inventory, marking entries verified so
   healthy games are not re-created or skipped as phantoms.
3. Backfills each draft with complete data (images, description, article,
   requirements, download links, SEO fields), `action_limit` per run.

Run it once after enabling the key, or any time state and site disagree.

### The reset action (delete everything and relist from zero)

`--action reset` (or the `autogamivoid-reset.yml` workflow) is the nuclear
option. It deletes **every** listing on the site — drafts and published — and
wipes `state.json`, so the next catalog run relists everything from zero with
current code and data. Use it when existing listings are stale or incomplete
and you want a clean relaunch rather than incremental patches:

1. Lists every owner listing (drafts included).
2. Deletes each one (`DELETE /api/admin/games/{slug}`); comments and bookmarks
   go with them (uploaded R2 images are not removed).
3. Clears `state.json` and rewrites the manifest as empty.
4. With the workflow's `relist` box ticked (default), the catalog run starts
   immediately after and republishes everything with real download links and
   Steam CDN images.

The workflow requires typing **RESET** into the `confirm` input before it
runs. The catalog action respects `action_limit`, so a large site relists over
several scheduled runs until done.

## Safety model

- **Writes**: one POST per new game, one PATCH per changed download. Nothing else.
- **Reads**: taxonomy fetch + catalog/fresh pages. Already-published games are
  skipped without any API call.
- **Retries**: 429/5xx get backoff with jitter; failures skip the game and continue.
- **Rate pacing**: `batch.request_pause_ms` (default 250ms) between gamivoid calls.
- **Slug stability**: slugs derive from the title and are the join key between
  state.json and gamivoid.
- **Bounded runs**: `batch.action_limit` + `seconds_limit` stop each run cleanly;
  the next scheduled run continues where it left off.

## Cloudflare: letting the automation through

If runs fail with a **Cloudflare bot challenge** ("Just a moment..."), GitHub's
runner IPs are being challenged by gamivoid.site's Cloudflare settings. Since
the site is your own zone, fix it in the Cloudflare dashboard:

1. **Security → Bot Fight Mode → Off** (it cannot be bypassed by rules on the
   free plan, and it blocks datacenter IPs wholesale).
2. **Security → WAF → Custom rules → Create rule**:
   - Name: `Allow API to automation`
   - Expression: `(http.host eq "gamivoid.site" and starts_with(http.request.uri.path, "/api/"))`
   - Action: **Skip** — enable skipping for Bot Fight-derived managed rules and
     super bot fight mode where available.
   - Order it **above** any other rules.
3. Alternatively (no Cloudflare change): run the automation from a machine with
   a trusted IP — e.g. a free-tier VPS with `autogamivoid` on a cron, using the
   same `config.json` shape.

The client also sends browser-style headers and detects challenge pages,
reporting a clear error instead of raw HTML when a block happens.

## Notes on sources

Both sites are WordPress-ish listings without public APIs. If a site changes
markup or blocks datacenter IPs (GitHub Actions runners), expect empty scrapes —
the run fails closed with a clear error rather than publishing garbage. Optional
per-source custom headers can be set in `sources.*.headers`.

## Development

```bash
cargo check  -p autogamivoid
cargo test   -p autogamivoid
cargo clippy -p autogamivoid --all-targets -- -D warnings
```
