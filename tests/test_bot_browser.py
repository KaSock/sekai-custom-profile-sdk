"""Protocol, lifecycle and resource tests; no Chromium or Rust build required."""

import asyncio
import base64
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import AsyncMock, Mock, patch

from aiohttp import ClientSession, web
from aiohttp.test_utils import TestClient, TestServer

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from bot_browser import BrowserWorker, MEDIA_LIMIT, font_path, is_software_gpu, safe_parts
from bot_service import create_app


class BrowserTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.md = self.root / 'cn'
        self.jp = self.root / 'jp'
        self.fonts = self.root / 'fonts'
        for directory in (self.md, self.jp, self.fonts):
            directory.mkdir()
        self.cfg = {
            'font_dir': str(self.fonts), 'bot_root': str(self.root), 'atlas_dir': str(self.root),
            'bot_url': 'http://127.0.0.1:8080', 'executable': 'not-used.exe',
            'token': 'test-only-token', 'source_url': 'https://example.invalid/source',
            'regions': ['cn'], 'browser_sdk_dir': str(self.root / 'missing-dist'),
        }
        self.native = types.SimpleNamespace(
            md_dir=self.md, fallback_md_dir=self.jp,
            signature=lambda: ('md-v1',), atlas_signature=lambda: 'fonts-v1',
            close=AsyncMock(), render=AsyncMock(return_value={'items': [], 'failures': []}),
        )
        self.worker = BrowserWorker(self.cfg, 'cn', self.native)
        self.clients = []

    async def asyncTearDown(self):
        for client in reversed(self.clients):
            await client.close()
        await self.worker.close()
        self.directory.cleanup()

    async def client(self, app):
        client = TestClient(TestServer(app))
        await client.start_server()
        self.clients.append(client)
        return client

    async def test_native_backend_still_starts_without_playwright(self):
        client = await self.client(create_app(self.cfg))
        response = await client.get('/health')
        self.assertEqual((await response.json())['backend'], 'native')

    async def test_browser_backend_preserves_health_and_request_auth(self):
        client = await self.client(create_app({**self.cfg, 'backend': 'browser'}))
        status = await (await client.get('/health')).json()
        self.assertEqual(status['backend'], 'browser')
        self.assertFalse(status['workers']['cn']['started'])
        self.assertEqual((await client.post('/render', json={})).status, 403)

    async def test_browser_response_preserves_the_bot_media_contract(self):
        item = {'seq': 1, 'kind': 'image', 'content_type': 'image/jpeg',
                'bytes': 4, 'data': 'anBlZw==', 'backend': 'browser'}
        self.worker.render = AsyncMock(return_value={'items': [item], 'failures': []})
        with patch('bot_browser.BrowserWorker', return_value=self.worker):
            client = await self.client(create_app({**self.cfg, 'backend': 'browser'}))
        profile = {'userCustomProfileCards': [{'seq': 1}]}
        response = await client.post('/render', json={'region': 'cn', 'profile': profile},
                                     headers={'Authorization': 'Bearer test-only-token'})
        self.assertEqual(response.status, 200)
        result = await response.json()
        self.assertEqual(result['data']['items'], [item])
        self.assertEqual(result['backend'], 'browser')
        self.assertEqual(set(result['timing']), {'queue_wait_ms', 'compute_ms', 'total_ms'})
        self.worker.render.assert_awaited_once_with(profile)

    def fake_playwright(self, gpu):
        self.worker.sdk.mkdir()
        for name in ('index.js', 'worker.js', 'sekai_profile_wasm.js', 'sekai_profile_wasm.wasm'):
            (self.worker.sdk / name).write_bytes(b'test-only-artifact')
        page = types.SimpleNamespace(
            on=Mock(), goto=AsyncMock(), evaluate=AsyncMock(return_value=gpu))
        context = types.SimpleNamespace(new_page=AsyncMock(return_value=page))
        browser = types.SimpleNamespace(new_context=AsyncMock(return_value=context), close=AsyncMock())
        engine = types.SimpleNamespace(
            chromium=types.SimpleNamespace(launch=AsyncMock(return_value=browser)), stop=AsyncMock())
        api = types.ModuleType('playwright.async_api')
        api.async_playwright = lambda: types.SimpleNamespace(start=AsyncMock(return_value=engine))
        modules = {'playwright': types.ModuleType('playwright'), 'playwright.async_api': api}
        return modules, page, browser, engine

    async def test_browser_start_uses_new_headless_and_serves_no_bot_credentials(self):
        gpu = {'unmaskedRenderer': 'ANGLE (AMD Radeon RX 580, D3D11)'}
        modules, page, _browser, engine = self.fake_playwright(gpu)
        with patch.dict(sys.modules, modules):
            await self.worker.start(self.worker.signature())
        engine.chromium.launch.assert_awaited_once_with(headless=True, channel='chromium')
        self.assertEqual(len(self.worker.status()['gpu']), 2)
        url = page.goto.call_args.args[0]
        self.assertTrue(url.startswith('http://127.0.0.1:'))
        async with self.worker.http.get(url) as response:
            html = await response.text()
        self.assertIn('window.alliumReady', html)
        self.assertNotIn(self.cfg['token'], html)
        self.assertNotIn(str(self.root), html)

    async def test_software_gpu_start_failure_closes_the_partial_browser(self):
        modules, _page, browser, engine = self.fake_playwright(
            {'unmaskedRenderer': 'ANGLE (Vulkan SwiftShader)'})
        with patch.dict(sys.modules, modules):
            with self.assertRaisesRegex(RuntimeError, 'did not confirm hardware'):
                await self.worker.render({'userCustomProfileCards': [{'seq': 1}]})
        browser.close.assert_awaited_once()
        engine.stop.assert_awaited_once()
        self.assertIsNone(self.worker.runner)
        self.assertIsNone(self.worker.http)
        self.assertEqual(self.worker.status()['gpu'], [])

    async def test_timeout_closes_browser_before_a_later_request_can_reuse_it(self):
        browser = types.SimpleNamespace(close=AsyncMock())
        self.worker.browser = browser
        self.worker.config['render_timeout_seconds'] = 0.01

        async def blocked(_profile):
            await asyncio.Event().wait()

        self.worker._render = blocked
        with self.assertRaises(asyncio.TimeoutError):
            await self.worker.render({'userCustomProfileCards': [{'seq': 1}]})
        browser.close.assert_awaited_once()
        self.assertIsNone(self.worker.browser)

    async def test_missing_sdk_is_explicit_and_closes_partial_state(self):
        with self.assertRaisesRegex(RuntimeError, 'Browser SDK files missing'):
            await self.worker.render({'userCustomProfileCards': [{'seq': 1}]})
        self.native.close.assert_awaited()
        self.assertFalse(self.worker.status()['started'])

    async def test_masterdata_has_the_same_restricted_jp_fallback(self):
        name = 'customProfileMaterialResources'
        (self.jp / (name + '.json')).write_text('[{"id":42}]', encoding='utf-8')
        app = web.Application()
        app.router.add_get('/md/{name}', self.worker.masterdata)
        client = await self.client(app)
        response = await client.get('/md/' + name)
        self.assertEqual(await response.json(), [{'id': 42}])
        self.assertEqual((await client.get('/md/secrets')).status, 404)
        self.assertEqual((await client.get('/md/..%5Csecret')).status, 400)

    async def test_resource_requests_deduplicate_and_keep_bot_token_server_side(self):
        calls = []
        png = b'\x89PNG\r\n\x1a\n' + b'fake-image-payload'

        async def upstream(request):
            calls.append((request.path, request.headers.get('Authorization')))
            await asyncio.sleep(0.01)
            return web.Response(body=png, content_type='image/png')

        bot = web.Application()
        bot.router.add_get('/internal/profile-renderer/cn/assets/card/test.png', upstream)
        bot_client = await self.client(bot)
        self.worker.config['bot_url'] = str(bot_client.make_url('')).rstrip('/')
        self.worker.http = ClientSession()
        bridge = web.Application()
        bridge.router.add_get('/resource/{kind}/{key:.*}', self.worker.resource)
        client = await self.client(bridge)
        first, second = await asyncio.gather(
            client.get('/resource/assets/card/test'), client.get('/resource/assets/card/test'))
        self.assertEqual(await first.read(), png)
        self.assertEqual(await second.read(), png)
        third = await client.get('/resource/assets/card/test')
        self.assertEqual(await third.read(), png)
        self.assertEqual(calls, [('/internal/profile-renderer/cn/assets/card/test.png',
                                  'Bearer test-only-token')])
        invalid = await client.get('/resource/assets/x%5C..%5Csecret')
        self.assertEqual(invalid.status, 400)

    async def test_static_pages_stay_ordered_and_only_animated_pages_use_native(self):
        jpeg = base64.b64encode(b'jpeg').decode()
        self.native.render.return_value = {
            'items': [{'seq': 2, 'kind': 'video', 'bytes': 3, 'data': 'bXA0'}], 'failures': [],
        }

        async def evaluate(_code, payload):
            if '.classify(' in _code:
                return {'pages': [{'index': index, 'animated': page['seq'] == 2}
                                  for index, page in enumerate(payload['profile']['userCustomProfileCards'])],
                        'timing': {'classify_ms': 1}}
            seq = payload['profile']['userCustomProfileCards'][0]['seq']
            self.assertNotEqual(seq, 2, 'animation candidate must skip scene preparation')
            if seq == 4:
                raise RuntimeError('bad page')
            return {
                'animated': seq == 2, 'timing': {'total_ms': 1},
                'item': {'seq': seq, 'kind': 'image', 'bytes': 4, 'data': jpeg, 'backend': 'browser'},
            }

        self.worker.start = AsyncMock()
        self.worker.slots.put_nowait({'page': types.SimpleNamespace(evaluate=evaluate)})
        profile = {'userId': 'test-player', 'userCustomProfileCards': [
            {'seq': 3}, {'seq': 2}, {'seq': 1}, {'seq': 4},
        ]}
        result = await self.worker._render(profile)
        self.assertEqual([item['seq'] for item in result['items']], [1, 2, 3])
        self.assertEqual(result['items'][1]['backend'], 'native-animation')
        self.assertEqual(result['failures'], [{'seq': 4, 'error': 'bad page'}])
        self.assertEqual(self.native.render.call_args.args[0]['userCustomProfileCards'], [{'seq': 2}])
        self.assertEqual(self.native.render.call_args.args[0]['userId'], 'test-player')

    async def test_native_animation_and_static_browser_work_overlap(self):
        native_started, static_finished = asyncio.Event(), asyncio.Event()

        async def native_render(profile):
            native_started.set()
            await static_finished.wait()
            return {'items': [{'seq': 2, 'bytes': 3}], 'failures': []}

        async def evaluate(code, _payload):
            if '.classify(' in code:
                return {'pages': [{'index': 1, 'animated': True}], 'timing': {}}
            await native_started.wait()
            static_finished.set()
            return {'animated': False, 'timing': {}, 'item': {'seq': 1, 'bytes': 4}}

        self.native.render.side_effect = native_render
        self.worker.start = AsyncMock()
        self.worker.slots.put_nowait({'page': types.SimpleNamespace(evaluate=evaluate)})
        result = await asyncio.wait_for(self.worker._render(
            {'userCustomProfileCards': [{'seq': 1}, {'seq': 2}]}), 2)
        self.assertEqual([item['seq'] for item in result['items']], [1, 2])

    async def test_cancel_drains_both_backends_before_closing(self):
        native_started, static_started = asyncio.Event(), asyncio.Event()
        cancelled = set()

        async def native_render(_profile):
            native_started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.add('native')

        async def evaluate(code, _payload):
            if '.classify(' in code:
                return {'pages': [{'index': 1, 'animated': True}], 'timing': {}}
            static_started.set()
            try:
                await asyncio.Event().wait()
            finally:
                cancelled.add('browser')

        self.native.render.side_effect = native_render
        self.worker.start = AsyncMock()
        self.worker.slots.put_nowait({'page': types.SimpleNamespace(evaluate=evaluate)})
        job = asyncio.create_task(self.worker.render({'userCustomProfileCards': [{'seq': 1}, {'seq': 2}]}))
        await asyncio.wait_for(asyncio.gather(native_started.wait(), static_started.wait()), 2)
        job.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await job
        self.assertEqual(cancelled, {'native', 'browser'})
        self.native.close.assert_awaited_once()

    async def test_classification_error_uses_full_scene_and_late_native_fallback(self):
        async def evaluate(code, _payload):
            if '.classify(' in code:
                raise RuntimeError('old bridge')
            return {'animated': True, 'timing': {}}

        self.worker.start = AsyncMock()
        self.worker.slots.put_nowait({'page': types.SimpleNamespace(evaluate=evaluate)})
        await self.worker._render({'userCustomProfileCards': [{'seq': 2}]})
        self.native.render.assert_awaited_once()

    async def test_combined_backends_share_response_budget_in_seq_order(self):
        async def evaluate(code, _payload):
            if '.classify(' in code:
                return {'pages': [{'index': 0, 'animated': True}], 'timing': {}}
            return {'animated': False, 'timing': {}, 'item': {'seq': 1, 'bytes': MEDIA_LIMIT}}

        self.native.render.return_value = {'items': [{'seq': 2, 'bytes': 1}], 'failures': []}
        self.worker.start = AsyncMock()
        self.worker.slots.put_nowait({'page': types.SimpleNamespace(evaluate=evaluate)})
        result = await self.worker._render({'userCustomProfileCards': [{'seq': 2}, {'seq': 1}]})
        self.assertEqual([item['seq'] for item in result['items']], [1])
        self.assertEqual(result['failures'][0]['seq'], 2)

    async def test_media_limit_matches_native_protocol(self):
        self.worker.start = AsyncMock()

        async def evaluate(_code, payload):
            if '.classify(' in _code:
                return {'pages': [{'index': 0, 'animated': False}], 'timing': {}}
            return {'animated': False, 'timing': {}, 'item': {
                'seq': 1, 'bytes': MEDIA_LIMIT + 1, 'data': '', 'kind': 'image',
            }}

        self.worker.slots.put_nowait({'page': types.SimpleNamespace(evaluate=evaluate)})
        result = await self.worker._render({'userCustomProfileCards': [{'seq': 1}]})
        self.assertEqual(result['items'], [])
        self.assertIn('64 MiB', result['failures'][0]['error'])

    def test_fonts_prefer_region_and_reject_traversal(self):
        (self.fonts / 'cn').mkdir()
        name = 'FOT-RodinNTLGPro-DB.ttf'
        (self.fonts / name).write_bytes(b'shared')
        regional = self.fonts / 'cn' / name
        regional.write_bytes(b'regional')
        self.assertEqual(font_path(self.fonts, 'cn', 'FZLanTingHei-DB-GBK'), regional.resolve())
        with self.assertRaises(web.HTTPBadRequest):
            font_path(self.fonts, 'cn', '../secret')
        for key in ('../secret', 'x/../secret', '/absolute', 'C:/secret', 'a\\b'):
            with self.assertRaises(web.HTTPBadRequest):
                safe_parts(key)

    def test_software_and_unconfirmed_gpu_are_not_reported_as_hardware(self):
        self.assertTrue(is_software_gpu({'unmaskedRenderer': 'ANGLE (Google, Vulkan SwiftShader)'}))
        self.assertTrue(is_software_gpu({'unmaskedRenderer': 'Microsoft Basic Render Driver'}))
        self.assertTrue(is_software_gpu({'unmaskedRenderer': None}))
        self.assertFalse(is_software_gpu({'unmaskedRenderer': 'ANGLE (AMD Radeon RX 580, D3D11)'}))


if __name__ == '__main__':
    unittest.main()
