<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, describeSync, relTime, type SyncStatus } from './api';
  import Backend from './Backend.svelte';

  const dispatch = createEventDispatcher<{ close: void; flash: string }>();

  let digestHour = 9;
  let autostart = true;
  let loaded = false;

  let sync: SyncStatus | null = null;
  let keyVisible = false;
  let syncing = false;
  let syncError = '';
  let backend: 'setup' | 'update' | null = null;
  let confirmingErase = false;
  let erasing = false;

  onMount(async () => {
    try {
      const p = await api.getPrefs();
      digestHour = p.digest_hour;
      autostart = p.autostart;
      sync = await api.getSyncStatus();
    } finally {
      loaded = true;
    }
  });

  async function refreshSync() {
    sync = await api.getSyncStatus();
  }

  async function save() {
    await api.setPrefs(digestHour, autostart);
    dispatch('flash', 'Settings saved');
    dispatch('close');
  }

  async function runSync() {
    syncing = true;
    syncError = '';
    try {
      const outcome = await api.syncNow();
      await refreshSync();
      dispatch('flash', describeSync(outcome));
    } catch (e) {
      syncError = String(e);
    } finally {
      syncing = false;
    }
  }

  async function copy(text: string | null, what: string) {
    if (!text) return;
    await navigator.clipboard.writeText(text);
    dispatch('flash', `${what} copied — save it in your password manager`);
  }

  async function erase() {
    erasing = true;
    syncError = '';
    try {
      await api.eraseBackup();
      await refreshSync();
      confirmingErase = false;
      dispatch('flash', 'Cloud backup erased and disconnected');
    } catch (e) {
      syncError = String(e);
    } finally {
      erasing = false;
    }
  }

  async function disconnect() {
    await api.disconnectBackend();
    await refreshSync();
    dispatch('flash', 'Disconnected — your watch list and backup are untouched');
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape' && !backend) dispatch('close');
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="scrim" role="presentation" on:click={() => dispatch('close')}>
  <div class="panel" role="dialog" on:click|stopPropagation>
    <h2>Settings</h2>
    {#if loaded}
      <label class="row">
        <span>Daily digest hour<br /><small>One notification per day while changes await review.</small></span>
        <select bind:value={digestHour}>
          {#each Array(24) as _, h}
            <option value={h}>{String(h).padStart(2, '0')}:00</option>
          {/each}
        </select>
      </label>
      <label class="row">
        <span>Launch at login<br /><small>Litecter starts hidden in the menu bar.</small></span>
        <input type="checkbox" bind:checked={autostart} />
      </label>

      <h3>Backup</h3>
      {#if sync?.configured}
        <div class="row">
          <span>
            {#if sync.failing_since}
              <strong class="failing">⚠ Backup is failing</strong><br />
              <small>
                Nothing has backed up since {relTime(sync.failing_since)}.
                {sync.last_synced_at
                  ? `Last good sync ${relTime(sync.last_synced_at)}.`
                  : 'No sync has ever succeeded.'}
              </small>
            {:else}
              Backing up to your own backend<br />
              <small>
                {sync.watched} URL(s) ·
                {sync.last_synced_at ? `synced ${relTime(sync.last_synced_at)}` : 'not yet synced'}
                · daily, and shortly after you change the list
              </small>
            {/if}
          </span>
          <button on:click={runSync} disabled={syncing}>
            {syncing ? 'Syncing…' : sync.failing_since ? 'Retry' : 'Sync now'}
          </button>
        </div>
        {#if sync.failing_since && sync.last_error}
          <p class="reason">{sync.last_error}</p>
        {/if}

        <div class="row stack">
          <code class="endpoint">{sync.endpoint}</code>
          {#if sync.worker_outdated}
            <!-- Behind is not broken. The wording has to say so, or a nag about
                 housekeeping reads as an outage. -->
            <p class="behind">
              Your backend runs
              {sync.deployed_worker_version === 0
                ? 'a version from before Litecter tracked them'
                : `v${sync.deployed_worker_version}`}; this build ships v{sync.bundled_worker_version}.
              Backups keep working meanwhile.
            </p>
            <button class="primary" on:click={() => (backend = 'update')}>
              Update backend worker…
            </button>
          {/if}
        </div>

        <div class="row stack">
          {#if keyVisible}
            <p class="label">This machine, and where it backs up — one paste:</p>
            <code class="key">{sync.link}</code>
            <p class="warn">
              The first half is your sync key. It is the only thing that can decrypt the backup, and
              nobody — not us, not Cloudflare — can recover it for you. Save it in your password
              manager.
            </p>
            <div class="inline">
              <button on:click={() => copy(sync?.link ?? null, 'Connection')}>Copy</button>
              <button on:click={() => (keyVisible = false)}>Hide</button>
            </div>
          {:else}
            <button on:click={() => (keyVisible = true)}>Show connection for another machine…</button>
          {/if}
        </div>

        <div class="row stack">
          {#if confirmingErase}
            <p class="warn">
              This deletes the backup from your bucket and disconnects this machine. Your watch list
              here is untouched. Nothing can undo it.
            </p>
            <div class="inline">
              <button class="danger" on:click={erase} disabled={erasing}>
                {erasing ? 'Erasing…' : 'Yes, erase the backup'}
              </button>
              <button on:click={() => (confirmingErase = false)}>Cancel</button>
            </div>
          {:else}
            <div class="inline">
              <button on:click={disconnect}>Stop syncing here</button>
              <button class="quiet" on:click={() => (confirmingErase = true)}>
                Delete cloud backup…
              </button>
            </div>
          {/if}
        </div>
      {:else}
        <div class="row stack">
          <p class="lede">
            Your watch list lives only on this Mac. A lost disk takes it with it — so Litecter can
            keep an encrypted copy in a backend you deploy to your own Cloudflare account, free, in
            about a minute.
          </p>
          <small>
            Snapshots stay local; only the watch list, your settings and anything unreviewed are
            uploaded. They're encrypted here, with a key the backend never sees.
          </small>
          <div class="inline">
            <button class="primary" on:click={() => (backend = 'setup')}>Set up backup</button>
          </div>
          {#if sync?.key}
            <p class="note">
              This machine already has a key but no backend to send it to — pick up where you left
              off above.
            </p>
          {/if}
        </div>
      {/if}
      {#if syncError}
        <p class="error">{syncError}</p>
      {/if}

      <div class="actions">
        <button on:click={() => dispatch('close')}>Cancel</button>
        <button class="primary" on:click={save}>Save</button>
      </div>
    {:else}
      <p class="loading">Loading…</p>
    {/if}
  </div>
</div>

{#if backend}
  <Backend
    mode={backend}
    {sync}
    on:close={() => (backend = null)}
    on:changed={refreshSync}
    on:flash={(e) => dispatch('flash', e.detail)}
  />
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 40;
  }
  .panel {
    width: min(460px, 92vw);
    max-height: 92vh;
    overflow-y: auto;
    background: var(--panel);
    border-radius: 12px;
    padding: 20px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  h2 {
    margin: 0 0 16px;
    font-size: 16px;
  }
  .row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 16px;
    padding: 10px 0;
    border-bottom: 1px solid var(--line);
  }
  small {
    color: var(--muted);
  }
  select,
  button {
    font: inherit;
    padding: 6px 10px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }
  .primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .danger {
    background: var(--red);
    border-color: var(--red);
    color: #fff;
  }
  .quiet {
    border-color: transparent;
    color: var(--muted);
  }
  .loading {
    color: var(--muted);
  }
  h3 {
    margin: 20px 0 4px;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--muted);
  }
  .row.stack {
    display: block;
  }
  .inline {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 10px;
    flex-wrap: wrap;
  }
  .lede {
    margin: 0 0 6px;
  }
  .label {
    margin: 0 0 6px;
    font-size: 12px;
    color: var(--muted);
  }
  .key,
  .endpoint {
    display: block;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12px;
    line-height: 1.6;
    word-break: break-all;
    background: var(--bg, rgba(127, 127, 127, 0.12));
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 10px;
    user-select: all;
  }
  .endpoint {
    color: var(--muted);
  }
  .behind {
    color: var(--warn);
    font-size: 12px;
    margin: 10px 0 0;
    line-height: 1.5;
  }
  .warn,
  .note {
    color: var(--muted);
    font-size: 12px;
    margin: 8px 0 0;
    line-height: 1.5;
  }
  .error {
    color: #d9534f;
    font-size: 12px;
    margin: 10px 0 0;
    word-break: break-word;
  }
  .failing {
    color: var(--warn);
  }
  .reason {
    color: var(--muted);
    font-size: 12px;
    margin: 8px 0 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    word-break: break-word;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
