// In-app auto-update, driven by Tauri's updater plugin. The plugin fetches the
// GitHub `latest.json` manifest (endpoint + pubkey configured in
// tauri.conf.json), and on install downloads the signed `.app.tar.gz`, verifies
// it against the pubkey, swaps the bundle in place, and — via the process
// plugin's relaunch() — restarts into the new version. One click, no browser,
// no re-download, no drag-to-Applications.
//
// State lives in a module-level store rather than per-component: the Updates UI
// lives in the settings modal, which is unmounted most of the time, but the
// gear's badge dot in App.svelte has to know about a pending update whether or
// not that modal has ever been opened.
//
// The release pipeline (.github/workflows/release.yml) publishes the updater
// artifacts + latest.json alongside the DMG; see docs/auto-update.md for the
// whole story.
import { derived, get, writable } from 'svelte/store';
import { getVersion } from '@tauri-apps/api/app';
import { relaunch } from '@tauri-apps/plugin-process';
import { check, type Update } from '@tauri-apps/plugin-updater';

/** The rolling release page — manual-download fallback when auto-update fails. */
export const RELEASES_PAGE = 'https://github.com/boat-builder/litecter/releases/latest';

export type UpdatePhase =
  | 'checking' // querying the update manifest
  | 'uptodate' // no newer release
  | 'available' // a newer release exists, not yet installing
  | 'downloading' // fetching + verifying the new bundle
  | 'installing' // bundle swapped, about to relaunch
  | 'error'; // check or download failed

export interface UpdateState {
  phase: UpdatePhase;
  /** The running app version. */
  current: string;
  /** The available version, set when phase === 'available'. */
  latest: string | null;
  /** Release notes for the available version. */
  notes: string;
  /** Download progress in [0, 1] while phase === 'downloading'. */
  progress: number;
  /** A human-readable failure reason when phase === 'error'. */
  error: string | null;
}

export const update = writable<UpdateState>({
  phase: 'checking',
  current: '',
  latest: null,
  notes: '',
  progress: 0,
  error: null,
});

/** Drives the badge dot on the settings gear. */
export const updateAvailable = derived(update, ($u) => $u.phase === 'available');

// The Update handle check() returned. install() needs that exact object, so it
// is kept out of the store — the store holds only what the view renders.
let handle: Update | null = null;

function errMsg(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (typeof e === 'string') return e;
  return 'Update failed.';
}

/** Re-run the update check — mount, the 6-hourly re-check, or the manual button. */
export async function checkForUpdate(): Promise<void> {
  update.update((s) => ({ ...s, phase: 'checking', error: null }));
  try {
    const upd = await check(); // null when up to date
    handle = upd;
    update.update((s) => ({
      ...s,
      phase: upd ? 'available' : 'uptodate',
      latest: upd?.version ?? null,
      notes: upd?.body ?? '',
      error: null,
    }));
  } catch (e) {
    update.update((s) => ({ ...s, phase: 'error', error: errMsg(e) }));
  }
}

/** Download, verify, install and relaunch into the available update. */
export async function installUpdate(): Promise<void> {
  const upd = handle;
  if (!upd) return;
  let total = 0;
  let done = 0;
  update.update((s) => ({ ...s, phase: 'downloading', progress: 0, error: null }));
  try {
    await upd.downloadAndInstall((ev) => {
      if (ev.event === 'Started') {
        // contentLength is absent on a chunked response — hence the guard below.
        total = ev.data.contentLength ?? 0;
        done = 0;
      } else if (ev.event === 'Progress') {
        // Progress carries this chunk's length, not a running total.
        done += ev.data.chunkLength;
        update.update((s) => ({ ...s, progress: total ? done / total : 0 }));
      } else if (ev.event === 'Finished') {
        update.update((s) => ({ ...s, phase: 'installing', progress: 1 }));
      }
    });
    // Bundle swapped on disk — restart into the new version.
    await relaunch();
  } catch (e) {
    update.update((s) => ({ ...s, phase: 'error', error: errMsg(e) }));
  }
}

// Litecter lives in the menu bar for weeks at a time, so a launch-only check
// would mean a long-running copy never learns about a release. Re-check
// quietly, and only from a settled phase — never interrupt a download, and
// never clear an update the user has already been offered.
const RECHECK_MS = 6 * 60 * 60 * 1000;

/** Quiet check on mount plus the periodic re-check. Returns a teardown. */
export function startUpdateWatch(): () => void {
  void getVersion().then((v) => update.update((s) => ({ ...s, current: v })));
  void checkForUpdate();
  const timer = setInterval(() => {
    const { phase } = get(update);
    if (phase === 'uptodate' || phase === 'error') void checkForUpdate();
  }, RECHECK_MS);
  return () => clearInterval(timer);
}
