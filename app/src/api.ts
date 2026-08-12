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
};

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
