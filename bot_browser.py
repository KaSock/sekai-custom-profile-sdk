"""BROWSER-01: Optional, persistent WebGL2 backend for the existing BOT service."""

import asyncio
from collections import OrderedDict
import logging
from pathlib import Path
import re
import secrets
import time
from urllib.parse import quote

from aiohttp import ClientSession, ClientTimeout, web

LOG = logging.getLogger('allium-browser')
ROOT = Path(__file__).resolve().parent
MEDIA_LIMIT = 64 * 1024 * 1024
JP_FALLBACK_TABLES = {
    'customProfileCharacterIconResources', 'customProfileMaterialResources',
    'customProfileUserInterfaceIconResources',
}
# Keep the game's font filenames and CN aliases aligned with sdf/outline/mod.rs.
FONT_FILES = {
    'FZLanTingHei-DB-GBK': ('FOT-RodinNTLGPro-DB.ttf', 'FOT-RodinNTLGPro-DB.otf'),
    'FOT-RodinNTLGPro-DB': ('FOT-RodinNTLGPro-DB.ttf', 'FOT-RodinNTLGPro-DB.otf'),
    'FZZhengHei-EB-GBK': ('FOT-SkipProN-B.otf',),
    'FOT-SkipProN-B': ('FOT-SkipProN-B.otf',),
    'FZShaoEr-M11-JF': ('FOT-PopHappinessStd-EB.otf',),
    'FOT-PopHappinessStd-EB': ('FOT-PopHappinessStd-EB.otf',),
    'FOT-Yuruka Std UB': ('FOT-YurukaStd-UB.otf',),
    'Roboto': ('Roboto-Regular.ttf',),
    'Source Han Sans SC': ('SourceHanSansSC-Medium.otf', 'SourceHanSansSC-Regular.otf'),
    'SourceHanSansSC-Medium': ('SourceHanSansSC-Medium.otf',),
    **{f'Noto Sans CJK {region}': ('NotoSansCJK-Regular.ttc',)
       for region in ('JP', 'KR', 'SC', 'TC')},
}


def safe_parts(value):
    """Reject traversal before resolving any host filesystem or upstream path."""
    parts = value.split('/')
    if (not value or len(value) > 4096 or '\\' in value or ':' in value or '\x00' in value
            or any(part in ('', '.', '..') for part in parts)):
        raise web.HTTPBadRequest(text='Invalid resource path')
    return parts


def font_path(root, region, family):
    if not family or len(family) > 160 or any(c in family for c in '/\\:\x00'):
        raise web.HTTPBadRequest(text='Invalid font family')
    if family in ('.', '..'):
        raise web.HTTPBadRequest(text='Invalid font family')
    names = FONT_FILES.get(family, (family + '.ttf', family + '.otf'))
    root = Path(root).resolve()
    for directory in (root / region, root):
        for name in names:
            path = (directory / name).resolve()
            if root in path.parents and path.is_file():
                return path
    return None


def is_software_gpu(gpu):
    renderer = gpu.get('unmaskedRenderer') or ''
    software = ('swiftshader', 'llvmpipe', 'softpipe', 'software',
                'microsoft basic render', 'warp')
    return not renderer or any(name in renderer.lower() for name in software)


class BrowserWorker:
    def __init__(self, config, region, native):
        self.config, self.region, self.native = config, region, native
        self.lock = asyncio.Lock()
        self.page_count = int(config.get('browser_page_threads', 2))
        if not 1 <= self.page_count <= 8:
            raise ValueError('browser_page_threads must be between 1 and 8')
        self.quality = int(config.get('image_quality', 90))
        if not 1 <= self.quality <= 100:
            raise ValueError('image_quality must be between 1 and 100')
        self.sdk = Path(config.get('browser_sdk_dir', ROOT / 'runtime/browser-sdk/dist')).resolve()
        self.browser = self.context = self.playwright = self.runner = self.http = None
        self.pages = []
        self.slots = asyncio.Queue()
        self.revision = None
        self.asset_cache = OrderedDict()
        self.asset_bytes = 0
        self.asset_budget = max(0, int(config.get('asset_cache_mb', 256))) * 1024 * 1024
        self.asset_tasks = {}
        self.request_serial = 0

    def status(self):
        return {
            'started': self.browser is not None,
            'page_threads': self.page_count,
            'gpu': [slot['gpu'] for slot in self.pages],
        }

    def signature(self):
        return self.native.signature(), self.native.atlas_signature()

    async def start(self, revision):
        if self.browser is not None:
            return
        required = ('index.js', 'worker.js', 'sekai_profile_wasm.js', 'sekai_profile_wasm.wasm')
        missing = [name for name in required if not (self.sdk / name).is_file()]
        if missing:
            raise RuntimeError(
                f'Browser SDK files missing in {self.sdk}: {", ".join(missing)}; '
                'run build-browser-bot.ps1 or supply a complete browser dist directory')
        try:
            from playwright.async_api import async_playwright
        except ImportError as exc:
            raise RuntimeError('Browser backend needs: python -m pip install playwright; '
                               'python -m playwright install chromium --no-shell') from exc
        self.http = ClientSession(timeout=ClientTimeout(total=30))
        prefix = '/' + secrets.token_urlsafe(24)
        internal = web.Application()
        internal.router.add_get(prefix + '/', self.index)
        internal.router.add_get(prefix + '/bridge.mjs', self.bridge)
        internal.router.add_get(prefix + '/masterdata/{name}', self.masterdata)
        internal.router.add_get(prefix + '/font', self.font)
        internal.router.add_get(prefix + '/resource/{kind}/{key:.*}', self.resource)
        internal.router.add_static(prefix + '/sdk/', self.sdk, show_index=False)
        self.runner = web.AppRunner(internal, access_log=None)
        await self.runner.setup()
        site = web.TCPSite(self.runner, '127.0.0.1', 0)
        await site.start()
        host, port = self.runner.addresses[0][:2]
        base = f'http://{host}:{port}' + prefix + '/'
        self.playwright = await async_playwright().start()
        launch = {
            'headless': self.config.get('browser_headless', True),
            # The full Chromium/new-headless channel can use a real GPU.
            'channel': self.config.get('browser_channel', 'chromium'),
        }
        if self.config.get('browser_executable'):
            launch.pop('channel')
            launch['executable_path'] = self.config['browser_executable']
        self.browser = await self.playwright.chromium.launch(**launch)
        self.context = await self.browser.new_context(service_workers='block')
        for index in range(self.page_count):
            page = await self.context.new_page()
            page.on('console', lambda message: LOG.log(
                logging.WARNING if message.type in ('warning', 'error')
                else logging.INFO if message.type == 'info' else logging.DEBUG,
                '[%s] browser: %s', self.region, message.text))
            await page.goto(base, wait_until='load', timeout=120_000)
            gpu = await page.evaluate('async region => (await window.alliumReady).initialize(region)', self.region)
            if self.config.get('browser_require_hardware', True) and is_software_gpu(gpu):
                raise RuntimeError(
                    'Browser did not confirm hardware WebGL2: ' + str(gpu) +
                    '; check the GPU driver/session or set browser_require_hardware=false '
                    'only for software-rendering diagnostics')
            LOG.info('[%s] browser slot=%s GPU=%s', self.region, index, gpu)
            slot = {'page': page, 'gpu': gpu}
            self.pages.append(slot)
            self.slots.put_nowait(slot)
        self.revision = revision

    async def index(self, request):
        # No BOT credentials or filesystem paths enter the web page.
        return web.Response(text=(
            '<!doctype html><meta charset="utf-8"><canvas id="card"></canvas>'
            '<script type="module">'
            'window.alliumReady = Promise.all([import("./sdk/index.js"), import("./bridge.mjs")])'
            '.then(([sdk, bridge]) => bridge.createBridge(sdk.BrowserRenderer));'
            'window.alliumReady.catch(error => console.error(String(error)));'
            '</script>'
        ), content_type='text/html')

    async def bridge(self, request):
        return web.FileResponse(ROOT / 'browser_bot/bridge.mjs',
                                headers={'Cache-Control': 'no-store'})

    async def masterdata(self, request):
        name = request.match_info['name']
        if not re.fullmatch(r'[A-Za-z][A-Za-z0-9_]*', name):
            raise web.HTTPBadRequest(text='Invalid masterdata table')
        path = self.native.md_dir / (name + '.json')
        if not path.is_file() and name in JP_FALLBACK_TABLES and self.native.fallback_md_dir:
            path = self.native.fallback_md_dir / (name + '.json')
        if not path.is_file():
            raise web.HTTPNotFound(text='Masterdata table unavailable')
        return web.FileResponse(path, headers={'Cache-Control': 'no-store'})

    async def font(self, request):
        path = font_path(self.config['font_dir'], self.region, request.query.get('family', ''))
        if path is None:
            raise web.HTTPNotFound(text='Font file unavailable')
        return web.FileResponse(path)

    async def resource(self, request):
        kind, key = request.match_info['kind'], request.match_info['key']
        if kind not in ('assets', 'static'):
            raise web.HTTPBadRequest(text='Invalid resource namespace')
        safe_parts(key)
        identity = (kind, key)
        if identity in self.asset_cache:
            data = self.asset_cache.pop(identity)
            self.asset_cache[identity] = data
            return web.Response(body=data, content_type='image/png')
        task = self.asset_tasks.get(identity)
        if task is None:
            task = asyncio.create_task(self.fetch_resource(kind, key))
            self.asset_tasks[identity] = task
        try:
            # One scene being cancelled must not cancel another scene's fetch.
            data = await asyncio.shield(task)
        finally:
            if task.done() and self.asset_tasks.get(identity) is task:
                self.asset_tasks.pop(identity, None)
        return web.Response(body=data, content_type='image/png')

    async def fetch_resource(self, kind, key):
        filename = key if key.endswith('.png') else key + '.png'
        url = (self.config['bot_url'].rstrip('/') +
               f'/internal/profile-renderer/{self.region}/{kind}/' +
               '/'.join(quote(part, safe='') for part in filename.split('/')))
        async with self.http.get(url, headers={
            'Authorization': 'Bearer ' + self.config['token'],
        }, allow_redirects=False) as response:
            if response.status == 404:
                raise web.HTTPNotFound(text='Profile resource unavailable')
            if response.status != 200:
                raise web.HTTPBadGateway(text=f'BOT resource request failed: HTTP {response.status}')
            # The BOT serves PNGs. Bound a single encoded resource to 64 MiB.
            chunks, size = [], 0
            async for chunk in response.content.iter_chunked(64 * 1024):
                size += len(chunk)
                if size > MEDIA_LIMIT:
                    raise web.HTTPBadGateway(text='Profile resource exceeds 64 MiB')
                chunks.append(chunk)
            data = b''.join(chunks)
        if not data.startswith(b'\x89PNG\r\n\x1a\n'):
            raise web.HTTPBadGateway(text='BOT resource is not a PNG')
        if len(data) <= self.asset_budget:
            identity = (kind, key)
            previous = self.asset_cache.pop(identity, b'')
            self.asset_bytes -= len(previous)
            self.asset_cache[identity] = data
            self.asset_bytes += len(data)
            while self.asset_bytes > self.asset_budget:
                _, removed = self.asset_cache.popitem(last=False)
                self.asset_bytes -= len(removed)
        return data

    async def render(self, profile):
        try:
            return await asyncio.wait_for(
                self._render(profile),
                timeout=float(self.config.get('render_timeout_seconds', 480)),
            )
        except BaseException:
            # A cancelled evaluate may keep running inside Chromium. Close it
            # before another request can reuse its canvas or worker state.
            await self.close()
            raise

    async def _render(self, profile):
        revision = await asyncio.to_thread(self.signature)
        if self.browser is not None and self.revision != revision:
            await self.close()
        started = time.monotonic()
        await self.start(revision)
        startup_ms = round((time.monotonic() - started) * 1000)
        if startup_ms:
            LOG.info('[%s] browser startup/check time=%sms', self.region, startup_ms)
        self.request_serial += 1
        serial = self.request_serial
        pages = profile['userCustomProfileCards']

        async def render_one(index, raw_page):
            slot = await self.slots.get()
            seq = raw_page.get('seq') if isinstance(raw_page, dict) else None
            try:
                if not isinstance(raw_page, dict):
                    raise ValueError('invalid profile page: expected an object')
                single = {**profile, 'userCustomProfileCards': [raw_page]}
                result = await slot['page'].evaluate(
                    'async payload => (await window.alliumReady).render(payload)',
                    {'profile': single, 'quality': self.quality,
                     'documentKey': f'bot-browser-{serial}-{index}'},
                )
                LOG.info('[%s] page seq=%s browser timings=%s animated=%s',
                         self.region, seq, result['timing'], result['animated'])
                for warning in result.get('item', {}).get('warnings', []):
                    LOG.warning('[%s] page seq=%s %s', self.region, seq, warning)
                return raw_page, result, None
            except Exception as exc:
                LOG.warning('[%s] browser page seq=%s failed: %s',
                            self.region, seq, exc)
                return raw_page, None, str(exc)
            finally:
                self.slots.put_nowait(slot)

        results = await asyncio.gather(*(render_one(index, page) for index, page in enumerate(pages)))
        items, failures, animated = [], [], []
        for page, result, error in results:
            if error is not None:
                failures.append({'seq': page.get('seq') if isinstance(page, dict) else None, 'error': error})
            elif result['animated']:
                animated.append(page)
            else:
                items.append(result['item'])
        # Browser SDK exposes PNG snapshots, not the native MP4 exporter.
        # Never silently replace an animated page with a still image.
        if animated:
            LOG.info('[%s] exporting %s animated pages with the native backend',
                     self.region, len(animated))
            native = await self.native.render({**profile, 'userCustomProfileCards': animated})
            items.extend({**item, 'backend': 'native-animation'} for item in native['items'])
            failures.extend(native.get('failures', []))
        items.sort(key=lambda item: item.get('seq') if isinstance(item.get('seq'), (int, float))
                   else float('inf'))
        accepted, total_bytes = [], 0
        for item in items:
            if total_bytes + item['bytes'] > MEDIA_LIMIT:
                failures.append({'seq': item.get('seq'), 'error': 'profile media exceeds the 64 MiB response budget'})
                continue
            accepted.append(item)
            total_bytes += item['bytes']
        return {'items': accepted, 'failures': failures, 'browser_startup_ms': startup_ms}

    async def close(self):
        # Reset first, including partially initialized browsers after a failure.
        browser, self.browser = self.browser, None
        playwright, self.playwright = self.playwright, None
        runner, self.runner = self.runner, None
        http, self.http = self.http, None
        self.context = None
        self.pages.clear()
        self.slots = asyncio.Queue()
        self.revision = None
        tasks = list(self.asset_tasks.values())
        self.asset_tasks.clear()
        for task in tasks:
            task.cancel()
        await asyncio.gather(*tasks, return_exceptions=True)
        for resource, method in ((browser, 'close'), (playwright, 'stop'),
                                 (runner, 'cleanup'), (http, 'close'), (self.native, 'close')):
            if resource is not None:
                try:
                    await getattr(resource, method)()
                except Exception:
                    LOG.exception('[%s] browser cleanup failed: %s', self.region, method)
        self.asset_cache.clear()
        self.asset_bytes = 0
