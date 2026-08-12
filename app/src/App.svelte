<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { api, type ChangeItem, type UrlRow } from './api';
  import ChangesList from './ChangesList.svelte';
  import DiffPanel from './DiffPanel.svelte';
  import Library from './Library.svelte';
  import Prefs from './Prefs.svelte';

  let tab: 'changes' | 'library' = 'changes';
  let urls: UrlRow[] = [];
  let changes: ChangeItem[] = [];
  let addValue = '';
  let addSchedule = 'weekly';
  let pendingBulk: string[] = [];
  let addBusy = false;
  let openChange: ChangeItem | null = null;
  let showPrefs = false;
  let toast = '';
  let toastTimer: ReturnType<typeof setTimeout>;
  let addInput: HTMLInputElement;
  let library: Library;

  $: unseen = changes.filter((c) => !c.seen_at).length;

  function flash(msg: string) {
    toast = msg;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = ''), 3500);
  }

  async function refresh() {
    try {
      [urls, changes] = await Promise.all([api.listUrls(), api.listChanges(false)]);
    } catch (e) {
      flash(String(e));
    }
  }

  async function addNow() {
    const items = pendingBulk.length ? pendingBulk : addValue.split(/\s+/).filter(Boolean);
    if (!items.length) return;
    addBusy = true;
    try {
      const errors = await api.addUrls(items, addSchedule);
      addValue = '';
      pendingBulk = [];
      await refresh();
      flash(
        errors.length
          ? `Added ${items.length - errors.length}, skipped ${errors.length}`
          : items.length === 1
            ? 'Watching — first check queued'
            : `Watching ${items.length} URLs`
      );
      api.checkNow(null); // pick up baselines right away
    } catch (e) {
      flash(String(e));
    } finally {
      addBusy = false;
    }
  }

  function onAddPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData('text') ?? '';
    if (text.includes('\n')) {
      e.preventDefault();
      pendingBulk = text
        .split('\n')
        .map((s) => s.trim())
        .filter((s) => s && !s.startsWith('#'));
    }
  }

  async function markSeen(ids: number[]) {
    await api.markSeen(ids);
    await refresh();
  }

  async function openDiff(c: ChangeItem) {
    openChange = c;
  }

  function onKeydown(e: KeyboardEvent) {
    const typing =
      e.target instanceof HTMLInputElement ||
      e.target instanceof HTMLTextAreaElement ||
      e.target instanceof HTMLSelectElement;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'n') {
      e.preventDefault();
      addInput?.focus();
      return;
    }
    if (typing || openChange) return;
    if (e.key === '/') {
      e.preventDefault();
      tab = 'library';
      requestAnimationFrame(() => library?.focusSearch());
    }
  }

  onMount(() => {
    refresh();
    const unlisten = listen('litecter://refresh', refresh);
    const poll = setInterval(refresh, 30_000);
    return () => {
      unlisten.then((f) => f());
      clearInterval(poll);
    };
  });
</script>

<svelte:window on:keydown={onKeydown} />

<div class="shell">
  <header>
    <div class="brand" title="Litecter">◉</div>
    <form
      class="addbar"
      on:submit|preventDefault={addNow}
    >
      <input
        bind:this={addInput}
        bind:value={addValue}
        on:paste={onAddPaste}
        placeholder={pendingBulk.length
          ? `${pendingBulk.length} URLs ready — press Enter to add`
          : 'Paste or type a URL to watch…  (⌘N)'}
        spellcheck="false"
        autocomplete="off"
      />
      <select bind:value={addSchedule} title="Check frequency">
        <option value="hourly">hourly</option>
        <option value="daily">daily</option>
        <option value="weekly">weekly</option>
        <option value="monthly">monthly</option>
      </select>
      <button type="submit" disabled={addBusy || (!addValue.trim() && !pendingBulk.length)}>
        {addBusy ? 'Adding…' : 'Watch'}
      </button>
      {#if pendingBulk.length}
        <button type="button" class="ghost" on:click={() => (pendingBulk = [])}>clear</button>
      {/if}
    </form>
    <button class="ghost" title="Settings" on:click={() => (showPrefs = true)}>⚙</button>
  </header>

  <nav>
    <button class:active={tab === 'changes'} on:click={() => (tab = 'changes')}>
      Changes{#if unseen}<span class="badge">{unseen}</span>{/if}
    </button>
    <button class:active={tab === 'library'} on:click={() => (tab = 'library')}>
      Library <span class="count">{urls.length}</span>
    </button>
  </nav>

  <main>
    {#if tab === 'changes'}
      <ChangesList
        {changes}
        on:open={(e) => openDiff(e.detail)}
        on:seen={(e) => markSeen([e.detail])}
        on:seenAll={async () => {
          await api.markAllSeen();
          await refresh();
        }}
      />
    {:else}
      <Library
        bind:this={library}
        {urls}
        unseenIds={new Set(changes.filter((c) => !c.seen_at).map((c) => c.url_id))}
        on:refresh={refresh}
        on:flash={(e) => flash(e.detail)}
      />
    {/if}
  </main>

  {#if openChange}
    <DiffPanel
      change={openChange}
      on:close={() => (openChange = null)}
      on:seen={async (e) => {
        await markSeen([e.detail]);
        openChange = null;
      }}
    />
  {/if}

  {#if showPrefs}
    <Prefs on:close={() => (showPrefs = false)} on:flash={(e) => flash(e.detail)} />
  {/if}

  {#if toast}
    <div class="toast">{toast}</div>
  {/if}
</div>

<style>
  :global(*) {
    box-sizing: border-box;
  }
  :global(body) {
    margin: 0;
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    background: var(--bg);
    color: var(--fg);
    font-size: 13.5px;
    -webkit-font-smoothing: antialiased;
  }
  :global(:root) {
    --bg: #fafafa;
    --panel: #ffffff;
    --fg: #1c1c1e;
    --muted: #78787d;
    --line: #e5e5e8;
    --accent: #4f46e5;
    --accent-soft: #eef0fe;
    --green: #0a7d33;
    --green-bg: #e9f7ee;
    --red: #c73030;
    --red-bg: #fdeeee;
    --warn: #b45309;
  }
  @media (prefers-color-scheme: dark) {
    :global(:root) {
      --bg: #171719;
      --panel: #202023;
      --fg: #ececf0;
      --muted: #9a9aa3;
      --line: #323236;
      --accent: #818cf8;
      --accent-soft: #2a2b45;
      --green: #4ade80;
      --green-bg: #12291a;
      --red: #f87171;
      --red-bg: #331b1b;
      --warn: #fbbf24;
    }
  }

  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  .brand {
    color: var(--accent);
    font-size: 18px;
  }
  .addbar {
    display: flex;
    flex: 1;
    gap: 8px;
  }
  .addbar input {
    flex: 1;
    padding: 7px 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--bg);
    color: var(--fg);
    font-size: 13.5px;
    outline: none;
  }
  .addbar input:focus {
    border-color: var(--accent);
  }
  select,
  button {
    font: inherit;
    color: var(--fg);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 6px 12px;
    cursor: pointer;
  }
  button[type='submit'] {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  button:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .ghost {
    border-color: transparent;
    background: transparent;
    color: var(--muted);
  }
  nav {
    display: flex;
    gap: 4px;
    padding: 8px 14px 0;
  }
  nav button {
    border: none;
    background: transparent;
    color: var(--muted);
    padding: 7px 12px;
    border-radius: 8px 8px 0 0;
    display: flex;
    align-items: center;
    gap: 7px;
  }
  nav button.active {
    color: var(--fg);
    background: var(--panel);
    border: 1px solid var(--line);
    border-bottom-color: var(--panel);
    margin-bottom: -1px;
    font-weight: 600;
  }
  .badge {
    background: var(--accent);
    color: #fff;
    border-radius: 99px;
    font-size: 11px;
    padding: 1px 7px;
    font-weight: 600;
  }
  .count {
    color: var(--muted);
    font-size: 12px;
  }
  main {
    flex: 1;
    overflow-y: auto;
    border-top: 1px solid var(--line);
    background: var(--panel);
  }
  .toast {
    position: fixed;
    bottom: 18px;
    left: 50%;
    transform: translateX(-50%);
    background: var(--fg);
    color: var(--bg);
    padding: 8px 16px;
    border-radius: 8px;
    font-size: 13px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.25);
    z-index: 50;
  }
</style>
