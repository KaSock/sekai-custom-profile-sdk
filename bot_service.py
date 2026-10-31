"""Local HTTP/NDJSON bridge. Run independently of the BOT (Python >= 3.10)."""

import argparse
import asyncio
import contextlib
import hashlib
import json
import logging
import os
from pathlib import Path
import secrets
import time

from aiohttp import web

LOG = logging.getLogger('allium-service')
MAX_RESPONSE = 96 * 1024 * 1024


class Worker:
    def __init__(self, config, region):
        self.config = config
        self.region = region
        self.lock = asyncio.Lock()
        self.process = None
        self.stderr_task = None
        self.revision = None
        self.serial = 0
        self.masterdata_root = Path(config['bot_root']).resolve() / 'data/sekai/assets/masterdata'
        self.md_dir = self.masterdata_root / region
        self.fallback_md_dir = self.masterdata_root / 'jp' if region != 'jp' else None

    def masterdata_params(self):
        params = {'dir': str(self.md_dir)}
        if self.fallback_md_dir is not None:
            params.update({
                'fallback_dir': str(self.fallback_md_dir),
                'fallback_tables': [
                    'customProfileCharacterIconResources',
                    'customProfileMaterialResources',
                    'customProfileUserInterfaceIconResources',
                ],
            })
        return params

    def signature(self):
        dirs = [(self.region, self.md_dir)]
        if self.fallback_md_dir is not None:
            dirs.append(('jp', self.fallback_md_dir))
        return tuple((f'{region}/{p.name}', p.stat().st_mtime_ns, p.stat().st_size)
                     for region, directory in dirs for p in sorted(directory.glob('*.json')))

    def atlas_signature(self):
        digest = hashlib.sha256()
        for name in ('customProfileTextFonts', 'customProfileShapeResources'):
            digest.update((self.md_dir / f'{name}.json').read_bytes())
        font_root = Path(self.config['font_dir'])
        # Region-specific fonts take precedence over the shared directory.
        for directory in (font_root / self.region, font_root):
            if not directory.is_dir():
                continue
            digest.update(f'dir:{directory}'.encode())
            for path in sorted(directory.rglob('*')):
                if path.is_file():
                    digest.update(f'{path.relative_to(font_root)}:{path.stat().st_mtime_ns}:{path.stat().st_size}'.encode())
        return digest.hexdigest()

    async def log_stderr(self, process):
        while line := await process.stderr.readline():
            LOG.info('[%s] %s', self.region, line.decode('utf-8', errors='replace').rstrip())

    async def start(self):
        if self.process and self.process.returncode is None:
            return
        await self.close()
        cfg = self.config
        signature = await asyncio.to_thread(self.signature)
        if not signature:
            raise RuntimeError(f'No masterdata in {self.md_dir}; request through the BOT first')
        asset_base = f'{cfg["bot_url"].rstrip("/")}/internal/profile-renderer/{self.region}'
        args = [str(Path(cfg['executable']).resolve()), 'serve',
                '--region', self.region, '--masterdata', str(self.md_dir),
                '--font-dir', str(Path(cfg['font_dir']).resolve()),
                '--atlas-dir', str(Path(cfg['atlas_dir']).resolve() / self.region),
                '--assets-url', f'{asset_base}/assets',
                '--static-url', f'{asset_base}/static',
                '--asset-cache-mb', str(cfg.get('asset_cache_mb', 256))]
        env = dict(os.environ, SEKAI_PROFILE_ASSET_TOKEN=cfg['token'])
        atlas = Path(cfg['atlas_dir']).resolve() / self.region
        atlas_signature = await asyncio.to_thread(self.atlas_signature)
        marker = atlas / '.bot-prepared.json'
        try:
            prepared = json.loads(marker.read_text(encoding='utf-8'))
        except (OSError, ValueError):
            prepared = {}
        if prepared.get('signature') != atlas_signature:
            LOG.info('Preparing %s atlases; the first request may take longer', self.region)
            prepare_args = [args[0], 'prepare', *args[2:]]
            if prepared:
                prepare_args.append('--force')
            self.process = await asyncio.create_subprocess_exec(
                *prepare_args, stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.PIPE, env=env, limit=1024 * 1024,
            )
            self.stderr_task = asyncio.create_task(self.log_stderr(self.process))
            output = await self.process.stdout.read()
            code = await self.process.wait()
            await self.stderr_task
            self.stderr_task = None
            message = output.decode('utf-8', errors='replace').strip()
            LOG.info('Atlas preparation (%s): %s', self.region, message)
            if code or 'missing fonts: none' not in message or 'shape atlas: not built' in message:
                raise RuntimeError(f'Atlas preparation incomplete ({self.region}): {message}')
            atlas.mkdir(parents=True, exist_ok=True)
            marker.write_text(json.dumps({'signature': atlas_signature}), encoding='utf-8')
        self.process = await asyncio.create_subprocess_exec(
            *args, stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE, env=env, limit=MAX_RESPONSE,
        )
        self.stderr_task = asyncio.create_task(self.log_stderr(self.process))
        self.revision = signature
        info = await self.request('capabilities')
        if not info.get('animation_export') or not info.get('jpeg'):
            raise RuntimeError('Renderer lacks animation-export; build the adapted version')
        loaded = await self.request('reload_masterdata', self.masterdata_params())
        if loaded.get('missing_tables'):
            raise RuntimeError(f'Missing masterdata: {loaded["missing_tables"]}')
        LOG.info('Started %s renderer (pid=%s)', self.region, self.process.pid)

    async def request(self, method, params=None):
        self.serial += 1
        request_id = self.serial
        body = json.dumps({'id': request_id, 'method': method, 'params': params or {}},
                          ensure_ascii=False, separators=(',', ':')).encode('utf-8')
        try:
            self.process.stdin.write(body + b'\n')
            await self.process.stdin.drain()
            line = await self.process.stdout.readline()
            if not line:
                raise RuntimeError('Renderer exited unexpectedly; see its stderr log')
            result = json.loads(line)
            if result.get('id') != request_id:
                raise RuntimeError('Renderer response ID mismatch')
        except BaseException:
            # Cancellation/timeout must not leave a previous reply on stdout
            # where it could be mistaken for the next user's result.
            await self.close()
            raise
        if not result.get('ok'):
            raise RuntimeError(result.get('error', 'Unknown renderer error'))
        return result['result']

    async def render(self, profile):
        try:
            return await asyncio.wait_for(
                self._render(profile), timeout=float(self.config.get('render_timeout_seconds', 480)))
        except BaseException:
            await self.close()
            raise

    async def _render(self, profile):
        await self.start()
        revision = await asyncio.to_thread(self.signature)
        marker = Path(self.config['atlas_dir']) / self.region / '.bot-prepared.json'
        try:
            prepared = json.loads(marker.read_text(encoding='utf-8'))
        except (OSError, ValueError):
            prepared = {}
        if prepared.get('signature') != await asyncio.to_thread(self.atlas_signature):
            # Fonts can change independently of masterdata. Release mapped
            # atlases before preparing replacements (required on Windows).
            await self.close()
            await self.start()
        elif revision != self.revision:
            result = await self.request('reload_masterdata', self.masterdata_params())
            if result.get('missing_tables'):
                raise RuntimeError(f'Missing masterdata: {result["missing_tables"]}')
            self.revision = revision
        return await self.request('render_profile_media', {
            'profile': profile, 'preset': self.config.get('animation_preset', 'qq-v2'),
            'quality': int(self.config.get('image_quality', 90)),
        })

    async def close(self):
        process, self.process = self.process, None
        if process is not None:
            if process.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    process.kill()
            await process.wait()
        if self.stderr_task is not None:
            self.stderr_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self.stderr_task
            self.stderr_task = None
        self.revision = None


def create_app(cfg):
    regions = cfg.get('regions', ['cn', 'jp', 'tw', 'en'])
    if not regions or any(r not in ('cn', 'jp', 'tw', 'en', 'kr') for r in regions):
        raise ValueError('Invalid regions')
    if not cfg.get('token') or cfg['token'] == 'CHANGE_ME':
        raise ValueError('Set a random token, and use the same token in BOT config')
    if not cfg.get('source_url', '').startswith('https://'):
        raise ValueError('Set source_url to your renderer fork with this deployed source')
    from urllib.parse import urlparse
    bot_url = urlparse(cfg['bot_url'])
    if bot_url.scheme != 'http' or bot_url.hostname not in ('127.0.0.1', '::1', 'localhost'):
        raise ValueError('bot_url must be a loopback HTTP address')
    workers = {region: Worker(cfg, region) for region in regions}
    # Native rendering is CPU/memory intensive. This gate is separate from
    # each region's pipe lock; queued clients never write concurrently.
    gate = asyncio.Semaphore(max(1, int(cfg.get('max_concurrent_renders', 1))))
    max_pending = max(1, int(cfg.get('max_pending_requests', 8)))
    pending = 0
    app = web.Application(client_max_size=16 * 1024 * 1024)

    async def health(request):
        return web.json_response({'ok': True, 'regions': regions, 'pending': pending})

    async def render(request):
        nonlocal pending
        expected = f'Bearer {cfg["token"]}'.encode()
        if not secrets.compare_digest(request.headers.get('Authorization', '').encode(), expected):
            raise web.HTTPForbidden(text='Invalid renderer credential')
        if pending >= max_pending:
            raise web.HTTPTooManyRequests(text='Renderer queue is full')
        try:
            payload = await request.json()
        except (ValueError, UnicodeError) as exc:
            raise web.HTTPBadRequest(text='Invalid JSON') from exc
        if not isinstance(payload, dict):
            raise web.HTTPBadRequest(text='Expected an object')
        region, profile = payload.get('region'), payload.get('profile')
        if not isinstance(region, str) or region not in workers or not isinstance(profile, dict):
            raise web.HTTPBadRequest(text='Invalid region or profile')
        pages = profile.get('userCustomProfileCards')
        if not isinstance(pages, list) or not pages or len(pages) > 20:
            raise web.HTTPBadRequest(text='Expected 1 to 20 custom profile pages')
        # Reading a request body yields to other clients; check again here.
        # No await between this guard and increment, so reservation is atomic.
        if pending >= max_pending:
            raise web.HTTPTooManyRequests(text='Renderer queue is full')
        pending += 1
        start = time.monotonic()
        worker = workers[region]
        try:
            async with worker.lock:
                async with gate:
                    acquired = time.monotonic()
                    result = await worker.render(profile)
            return web.json_response({
                'ok': True, 'data': result, 'source_url': cfg['source_url'],
                'timing': {
                    'queue_wait_ms': round((acquired - start) * 1000),
                    'compute_ms': round((time.monotonic() - acquired) * 1000),
                    'total_ms': round((time.monotonic() - start) * 1000),
                },
            })
        except Exception as exc:
            LOG.exception('Rendering failed (%s)', region)
            return web.json_response({'ok': False, 'error': str(exc)}, status=500)
        finally:
            pending -= 1

    async def cleanup(_app):
        await asyncio.gather(*(worker.close() for worker in workers.values()))

    app.router.add_get('/health', health)
    app.router.add_post('/render', render)
    app.on_cleanup.append(cleanup)
    return app


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--config', type=Path, default=Path(__file__).with_name('bot_service.json'))
    args = parser.parse_args()
    cfg = json.loads(args.config.read_text(encoding='utf-8-sig'))
    # Relative runtime paths are relative to the config file, not the shell cwd.
    for key in ('executable', 'bot_root', 'font_dir', 'atlas_dir'):
        path = Path(cfg[key])
        if not path.is_absolute():
            cfg[key] = str((args.config.resolve().parent / path).resolve())
    logging.basicConfig(level=logging.INFO, format='%(asctime)s | %(levelname)s | %(message)s')
    web.run_app(create_app(cfg), host='127.0.0.1', port=int(cfg.get('port', 45557)),
                access_log=None)


if __name__ == '__main__':
    main()
