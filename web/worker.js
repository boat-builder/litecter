// litecter.cc — the marketing home page.
//
// A single static page served from the Worker itself (index.html is bundled as
// a Text module, see wrangler.toml `rules`), plus one redirect:
//
//   /download/mac -> the DMG asset on the newest GitHub Release
//
// The redirect targets GitHub's `releases/latest/download/<name>` alias, which
// always resolves to whatever the release workflow last published — so the
// download button never needs to be touched when a version ships. The version
// *label* under the button is looked up from the GitHub API and edge-cached, so
// a failed lookup degrades to "latest release" instead of breaking the page.

import HTML from './index.html';

const REPO = 'boat-builder/litecter';
const DMG_URL = `https://github.com/${REPO}/releases/latest/download/Litecter-macos-arm64.dmg`;
const VERSION_TTL = 1800; // seconds the release lookup stays warm at the edge

async function latestVersion() {
  try {
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers: { 'user-agent': 'litecter.cc', accept: 'application/vnd.github+json' },
      cf: { cacheTtl: VERSION_TTL, cacheEverything: true },
    });
    if (!res.ok) return null;
    const tag = (await res.json()).tag_name;
    return typeof tag === 'string' && /^v?\d+(\.\d+)*$/.test(tag) ? tag : null;
  } catch {
    return null; // page still renders, just without a version number
  }
}

export default {
  async fetch(request) {
    const url = new URL(request.url);

    // Canonical host: www and any other hostname fold into the apex.
    if (url.hostname !== 'litecter.cc' && url.hostname.endsWith('litecter.cc')) {
      url.hostname = 'litecter.cc';
      return Response.redirect(url.toString(), 301);
    }

    if (request.method !== 'GET' && request.method !== 'HEAD') {
      return new Response('Method not allowed', { status: 405, headers: { allow: 'GET, HEAD' } });
    }

    const path = url.pathname.replace(/\/+$/, '') || '/';

    if (path === '/download' || path === '/download/mac' || path === '/download/macos') {
      return Response.redirect(DMG_URL, 302);
    }

    if (path === '/robots.txt') {
      return new Response('User-agent: *\nAllow: /\n', {
        headers: { 'content-type': 'text/plain; charset=utf-8', 'cache-control': 'public, max-age=86400' },
      });
    }

    if (path !== '/') {
      return Response.redirect(new URL('/', url).toString(), 302);
    }

    const body = HTML.replace('__VERSION__', await latestVersion() ?? 'latest release');
    return new Response(body, {
      headers: {
        'content-type': 'text/html; charset=utf-8',
        'cache-control': 'public, max-age=300',
        'x-content-type-options': 'nosniff',
        'referrer-policy': 'strict-origin-when-cross-origin',
      },
    });
  },
};
