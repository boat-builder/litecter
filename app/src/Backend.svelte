<script lang="ts">
  // Setting up a backend and updating one are the same dialog because they are
  // the same three routes over the same artifact — only the text and the footer
  // differ. Keeping them apart would mean maintaining two copies of a tab strip
  // and letting them drift.
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, type Instructions, type Route, type SyncStatus } from './api';

  export let mode: 'setup' | 'update';
  export let sync: SyncStatus | null = null;

  const dispatch = createEventDispatcher<{ close: void; flash: string; changed: void }>();

  const ROUTES: { id: Route; label: string; blurb: string }[] = [
    { id: 'browser', label: 'Browser', blurb: 'Nothing to install. About a minute.' },
    { id: 'agent', label: 'AI agent', blurb: 'Paste one prompt into Claude Code or similar.' },
    { id: 'terminal', label: 'Terminal', blurb: 'Node and wrangler.' },
  ];

  let route: Route = 'browser';
  let instructions: Instructions | null = null;
  let loading = true;

  let url = '';
  let linkCode = '';
  let linking = false;
  let busy = false;
  let error = '';

  // Snapshotted when the dialog opens rather than read live. A card that
  // vanishes the instant a recheck succeeds reads as a bug; this one stays and
  // changes its face.
  let fromVersion: number | null = null;
  let nowVersion: number | null = null;
  let checking = false;

  $: target = sync?.bundled_worker_version ?? null;
  $: updated = mode === 'update' && nowVersion !== null && target !== null && nowVersion >= target;

  onMount(async () => {
    fromVersion = sync?.deployed_worker_version ?? null;
    await load();
  });

  async function load() {
    loading = true;
    error = '';
    try {
      instructions = await api.getInstructions(mode, route);
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  async function pick(next: Route) {
    route = next;
    await load();
  }

  async function copy(text: string, what: string) {
    await navigator.clipboard.writeText(text);
    dispatch('flash', `${what} copied`);
  }

  async function copyWorker() {
    await copy(await api.getWorkerSource(), 'Worker code');
  }

  async function connect() {
    busy = true;
    error = '';
    try {
      await api.connectBackend(url);
      dispatch('flash', 'Backend connected — backing up now');
      dispatch('changed');
      dispatch('close');
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function adopt() {
    busy = true;
    error = '';
    try {
      await api.adoptLink(linkCode);
      dispatch('flash', 'Connected to your other machine');
      dispatch('changed');
      dispatch('close');
    } catch (e) {
      error = String(e);
    } finally {
      busy = false;
    }
  }

  async function recheck() {
    checking = true;
    error = '';
    try {
      nowVersion = await api.checkWorker();
      dispatch('changed');
      if (!updated) error = `Your backend still reports v${nowVersion}. Did the deploy finish?`;
    } catch (e) {
      error = String(e);
    } finally {
      checking = false;
    }
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === 'Escape') dispatch('close');
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="scrim" role="presentation" on:click={() => dispatch('close')}>
  <div class="panel" role="dialog" on:click|stopPropagation>
    {#if mode === 'setup'}
      <h2>Set up backup</h2>
      <p class="lede">
        Litecter runs no sync service. You deploy a small backend — one file, no dependencies — to
        your own Cloudflare account. It stays yours: no account to make, nobody else holding your
        list, and comfortably inside the free plan.
      </p>
    {:else}
      <h2>Update your backend</h2>
      <p class="lede">
        A newer backend is available. Your backups keep working meanwhile — this is housekeeping,
        not a breakage. Replacing only the code leaves your bucket, your secret and your domain
        exactly as they are.
      </p>
    {/if}

    <div class="tabs" role="tablist">
      {#each ROUTES as r}
        <button
          role="tab"
          class:active={route === r.id}
          aria-selected={route === r.id}
          on:click={() => pick(r.id)}
        >
          {r.label}
          <small>{r.blurb}</small>
        </button>
      {/each}
    </div>

    <div class="body">
      {#if loading}
        <p class="muted">Loading…</p>
      {:else}
        {#if route === 'browser'}
          <div class="inline">
            <button class="primary" on:click={copyWorker}>Copy worker code</button>
            {#if mode === 'setup' && sync?.token}
              <button on:click={() => copy(sync.token ?? '', 'Token')}>Copy token</button>
            {/if}
          </div>
          {#if mode === 'setup'}
            <p class="note">
              The token is this machine's password to its own backup. It goes in the Worker as a
              secret named <code>SYNC_TOKEN</code>, and nowhere else.
            </p>
          {/if}
        {:else}
          <div class="inline">
            <button class="primary" on:click={() => copy(instructions?.text ?? '', 'Instructions')}>
              {route === 'agent' ? 'Copy prompt' : 'Copy commands'}
            </button>
            {#if instructions?.carries_secret}
              <span class="secret">⚠ contains your token — paste it only into your own tools</span>
            {:else}
              <span class="muted small">No secret in this text.</span>
            {/if}
          </div>
        {/if}

        <pre>{instructions?.text ?? ''}</pre>
      {/if}
    </div>

    {#if mode === 'setup'}
      <div class="foot">
        {#if linking}
          <label class="field">
            <span>Paste the connection string from your other machine</span>
            <input bind:value={linkCode} placeholder="KEY@https://…" spellcheck="false" />
          </label>
          <div class="inline">
            <button class="primary" on:click={adopt} disabled={busy || !linkCode.trim()}>
              {busy ? 'Checking…' : 'Connect'}
            </button>
            <button on:click={() => (linking = false)}>Back</button>
          </div>
        {:else}
          <label class="field">
            <span>Then paste the Worker's URL here</span>
            <input
              bind:value={url}
              placeholder="https://litecter-sync.you.workers.dev"
              spellcheck="false"
              on:keydown={(e) => e.key === 'Enter' && url.trim() && connect()}
            />
          </label>
          <div class="inline">
            <button class="primary" on:click={connect} disabled={busy || !url.trim()}>
              {busy ? 'Checking…' : 'Connect'}
            </button>
            <button on:click={() => (linking = true)}>I already set one up elsewhere</button>
          </div>
          <p class="note">
            Litecter checks the address answers before saving it — it can't see your Cloudflare
            account, so this is the only confirmation that means anything.
          </p>
        {/if}
      </div>
    {:else}
      <div class="foot">
        <div class="card" class:done={updated}>
          <span class="what">
            {#if updated}
              Updated ✓ — now running v{nowVersion}
            {:else}
              <code>{sync?.endpoint ?? 'your backend'}</code><br />
              <small>
                {fromVersion === 0 ? 'pre-versioning' : `v${fromVersion}`} → v{target}
              </small>
            {/if}
          </span>
          <button on:click={recheck} disabled={checking}>
            {checking ? 'Checking…' : updated ? 'Check again' : 'Check again'}
          </button>
        </div>
        {#if sync?.worker_needs_token_secret}
          <p class="note">
            This backend predates Litecter's token check, so it has no secret yet. After deploying
            the new code you'll need to add one — the steps above include it.
          </p>
        {/if}
      </div>
    {/if}

    {#if error}
      <p class="error">{error}</p>
    {/if}

    <div class="actions">
      <button on:click={() => dispatch('close')}>{mode === 'setup' ? 'Cancel' : 'Done'}</button>
    </div>
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
    z-index: 45;
    padding: 20px;
  }
  .panel {
    width: min(620px, 96vw);
    max-height: 92vh;
    overflow-y: auto;
    background: var(--panel);
    border-radius: 12px;
    padding: 20px;
    box-shadow: 0 12px 48px rgba(0, 0, 0, 0.35);
  }
  h2 {
    margin: 0 0 8px;
    font-size: 16px;
  }
  .lede {
    margin: 0 0 14px;
    color: var(--muted);
    line-height: 1.5;
  }
  .tabs {
    display: flex;
    gap: 6px;
    border-bottom: 1px solid var(--line);
  }
  .tabs button {
    flex: 1;
    display: block;
    text-align: left;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: 8px 8px 0 0;
    background: transparent;
    color: var(--muted);
    padding: 8px 10px;
    cursor: pointer;
    font: inherit;
  }
  .tabs button.active {
    color: var(--fg);
    background: var(--bg);
    border-color: var(--line);
    font-weight: 600;
  }
  .tabs small {
    display: block;
    font-weight: 400;
    font-size: 11px;
    color: var(--muted);
    margin-top: 2px;
  }
  .body {
    padding: 14px 0;
  }
  pre {
    margin: 12px 0 0;
    padding: 12px;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: 8px;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 11.5px;
    line-height: 1.55;
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 320px;
    overflow-y: auto;
  }
  .inline {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
  }
  button {
    font: inherit;
    padding: 6px 12px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--panel);
    color: var(--fg);
    cursor: pointer;
  }
  .primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .foot {
    border-top: 1px solid var(--line);
    padding-top: 14px;
  }
  .field {
    display: block;
    margin-bottom: 10px;
  }
  .field span {
    display: block;
    margin-bottom: 6px;
  }
  .field input {
    width: 100%;
    font: inherit;
    padding: 7px 10px;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--bg);
    color: var(--fg);
  }
  .card {
    display: flex;
    align-items: center;
    gap: 12px;
    justify-content: space-between;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: 9px;
    background: var(--bg);
  }
  .card.done {
    border-color: var(--green);
    color: var(--green);
  }
  .card code {
    font-size: 12px;
    word-break: break-all;
  }
  .note,
  .small {
    color: var(--muted);
    font-size: 12px;
    margin: 10px 0 0;
    line-height: 1.5;
  }
  .muted {
    color: var(--muted);
  }
  .secret {
    color: var(--warn);
    font-size: 12px;
  }
  .error {
    color: #d9534f;
    font-size: 12px;
    margin: 10px 0 0;
    word-break: break-word;
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    margin-top: 16px;
  }
</style>
