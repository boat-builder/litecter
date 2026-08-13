<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api } from './api';
  import { checkForUpdate, installUpdate, RELEASES_PAGE, update } from './updater';

  const dispatch = createEventDispatcher<{ close: void; flash: string }>();

  let digestHour = 9;
  let autostart = true;
  let loaded = false;

  // The status line under the update row: always says which version is running,
  // so "did my update land?" never needs a trip to the releases page.
  $: status = !$update.current
    ? 'Checking…'
    : $update.phase === 'available' || $update.phase === 'downloading'
      ? `Current: v${$update.current}`
      : $update.phase === 'installing'
        ? 'Restarting…'
        : $update.phase === 'error'
          ? `v${$update.current} · Couldn't check`
          : `v${$update.current} · Up to date`;

  onMount(async () => {
    try {
      const p = await api.getPrefs();
      digestHour = p.digest_hour;
      autostart = p.autostart;
    } finally {
      loaded = true;
    }
  });

  async function save() {
    await api.setPrefs(digestHour, autostart);
    dispatch('flash', 'Settings saved');
    dispatch('close');
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') dispatch('close');
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
    {:else}
      <p class="loading">Loading…</p>
    {/if}

    <!-- Outside the `loaded` gate on purpose: if prefs fail to load, the user
         must still be able to reach the update button. -->
    <div class="section">Updates</div>
    <div class="updates">
      {#if $update.phase === 'available'}
        <!-- The one click: names the version, promises the restart, and needs
             no further confirmation. -->
        <button class="primary wide" title={$update.notes} on:click={installUpdate}>
          ↓ Update to v{$update.latest} &amp; Restart
        </button>
      {:else if $update.phase === 'downloading' || $update.phase === 'installing'}
        <div class="progress" aria-live="polite">
          <span>
            {$update.phase === 'installing'
              ? 'Installing…'
              : `Downloading… ${Math.round($update.progress * 100)}%`}
          </span>
          <div class="track">
            <div class="fill" style:width="{Math.round($update.progress * 100)}%"></div>
          </div>
        </div>
      {:else}
        <button class="wide" disabled={$update.phase === 'checking'} on:click={checkForUpdate}>
          {$update.phase === 'checking' ? 'Checking…' : 'Check for updates'}
        </button>
      {/if}
      <p class="status" title={$update.error ?? ''}>{status}</p>
      {#if $update.phase === 'error'}
        <!-- Auto-update will fail for someone (proxy, read-only install
             location, disk full); a dead end is the worst outcome. -->
        <button class="link" on:click={() => api.openExternal(RELEASES_PAGE)}>
          Download manually…
        </button>
      {/if}
    </div>

    {#if loaded}
      <div class="actions">
        <button on:click={() => dispatch('close')}>Cancel</button>
        <button class="primary" on:click={save}>Save</button>
      </div>
    {/if}
  </div>
</div>

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
    width: min(440px, 92vw);
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
  .section {
    margin-top: 18px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .updates {
    padding-top: 8px;
  }
  .wide {
    width: 100%;
    text-align: center;
  }
  .progress {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px 0;
    color: var(--muted);
  }
  .track {
    height: 4px;
    border-radius: 99px;
    background: var(--line);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s ease-out;
  }
  .status {
    margin: 8px 0 0;
    font-size: 12px;
    color: var(--muted);
  }
  .link {
    border: none;
    background: transparent;
    padding: 2px 0;
    color: var(--accent);
    font-size: 12px;
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
  .loading {
    color: var(--muted);
  }
</style>
