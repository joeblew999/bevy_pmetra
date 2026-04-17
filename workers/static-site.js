// Cloudflare Worker — serves pmetra WASM app from R2.
// Bound to R2 bucket "pmetra-assets" via wrangler.toml.

const MIME_TYPES = {
  '.html': 'text/html',
  '.js': 'application/javascript',
  '.wasm': 'application/wasm',
  '.css': 'text/css',
  '.json': 'application/json',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.svg': 'image/svg+xml',
  '.txt': 'text/plain',
};

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    let key = url.pathname.slice(1) || 'index.html';

    const object = await env.ASSETS.get(key);
    if (!object) {
      // Try index.html for SPA routing.
      const fallback = await env.ASSETS.get('index.html');
      if (!fallback) return new Response('Not Found', { status: 404 });
      return new Response(fallback.body, {
        headers: { 'content-type': 'text/html', 'cache-control': 'no-cache' },
      });
    }

    const ext = '.' + key.split('.').pop();
    const contentType = MIME_TYPES[ext] || 'application/octet-stream';

    const headers = new Headers();
    headers.set('content-type', contentType);
    // Cache WASM and JS aggressively (hashed filenames).
    if (ext === '.wasm' || ext === '.js') {
      headers.set('cache-control', 'public, max-age=31536000, immutable');
    } else {
      headers.set('cache-control', 'public, max-age=60');
    }

    return new Response(object.body, { headers });
  },
};
