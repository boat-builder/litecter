import { invoke } from '@tauri-apps/api/core';

export interface UrlRow {
  id: number;
  url: string;
  title: string | null;
  schedule: string;
  selector: string | null;
  next_check_at: number;
  last_checked_at: number | null;
  status: string;
  error_message: string | null;
  error_count: number;
}

export interface ChangeItem {
  id: number;
  url_id: number;
  url: string;
  title: string | null;
  detected_at: number;
  seen_at: number | null;
  lines_added: number;
  lines_removed: number;
  snippet: string | null;
}

export interface Prefs {
  digest_hour: number;
  autostart: boolean;
}

export interface SyncStatus {
  /** Both halves present: a key *and* a backend to send it to. */
  configured: boolean;
  /** Present once a key exists. Shown only when the user asks to see it. */
  key: string | null;
  /** Key + backend as one string, for setting up a second machine. */
  link: string | null;
  /** What the backend's SYNC_TOKEN secret has to be. */
  token: string | null;
  last_synced_at: number | null;
  watched: number;
  endpoint: string | null;
  /** Set while the backup is broken; cleared as soon as one succeeds. */
  failing_since: number | null;
  last_error: string | null;
  /** The worker version this build ships. */
  bundled_worker_version: number | null;
  /**
   * What the last probe found, or null while unknown. Unknown is deliberately
   * not the same as outdated — an unreachable backend must not be nagged about.
   */
  deployed_worker_version: number | null;
  worker_outdated: boolean;
  /** A backend from before the token check: updating it needs one extra step. */
  worker_needs_token_secret: boolean;
}

export type Route = 'browser' | 'agent' | 'terminal';

export interface Instructions {
  text: string;
  /** Whether this text contains the token. Shown at the copy button. */
  carries_secret: boolean;
}

export interface SyncOutcome {
  urls: number;
  added: number;
  removed: number;
  pendings_restored: number;
  uploaded_bytes: number;
  diffs_dropped_for_size: number;
}

export const api = {
  listUrls: () => invoke<UrlRow[]>('list_urls'),
  addUrls: (urls: string[], every: string) => invoke<string[]>('add_urls', { urls, every }),
  removeUrls: (ids: number[]) => invoke<void>('remove_urls', { ids }),
  setSchedule: (ids: number[], every: string) => invoke<void>('set_schedule', { ids, every }),
  checkNow: (ids: number[] | null) => invoke<void>('check_now', { ids }),
  listChanges: (unseenOnly: boolean) => invoke<ChangeItem[]>('list_changes', { unseenOnly }),
  getDiff: (changeId: number) => invoke<string>('get_diff', { changeId }),
  markSeen: (changeIds: number[]) => invoke<void>('mark_seen', { changeIds }),
  markAllSeen: () => invoke<void>('mark_all_seen'),
  getPrefs: () => invoke<Prefs>('get_prefs'),
  setPrefs: (digestHour: number, autostart: boolean) =>
    invoke<void>('set_prefs', { digestHour, autostart }),
  getSyncStatus: () => invoke<SyncStatus>('get_sync_status'),
  getWorkerSource: () => invoke<string>('get_worker_source'),
  getInstructions: (kind: 'setup' | 'update', route: Route) =>
    invoke<Instructions>('get_instructions', { kind, route }),
  connectBackend: (url: string) => invoke<void>('connect_backend', { url }),
  adoptLink: (code: string) => invoke<void>('adopt_link', { code }),
  checkWorker: () => invoke<number | null>('check_worker'),
  eraseBackup: () => invoke<void>('erase_backup'),
  disconnectBackend: () => invoke<void>('disconnect_backend'),
  syncNow: () => invoke<SyncOutcome>('sync_now_cmd'),
};

/** Summarise a completed sync in one line of plain language. */
export function describeSync(o: SyncOutcome): string {
  const parts: string[] = [];
  if (o.added) parts.push(`${o.added} added`);
  if (o.removed) parts.push(`${o.removed} removed`);
  if (o.pendings_restored) parts.push(`${o.pendings_restored} change(s) restored`);
  const detail = parts.length ? ` — ${parts.join(', ')}` : '';
  return `Backed up ${o.urls} URL(s)${detail}`;
}

/** "2h ago" / "in 6d" / "just now" */
export function relTime(ts: number, now = Math.floor(Date.now() / 1000)): string {
  const past = ts <= now;
  const d = Math.abs(now - ts);
  if (d < 5) return 'just now';
  const unit =
    d < 60 ? `${d}s` : d < 3600 ? `${Math.floor(d / 60)}m` : d < 86400 ? `${Math.floor(d / 3600)}h` : `${Math.floor(d / 86400)}d`;
  return past ? `${unit} ago` : `in ${unit}`;
}

export function domainOf(url: string): string {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}
