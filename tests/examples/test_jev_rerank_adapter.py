"""Offline contract tests for the optional Jev choice adapter."""
import contextlib
import io
import json
from pathlib import Path
import runpy
import unittest
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[2] / 'docs/examples/jev-reranker-adapter/jev_rerank_shim_choice.py'
shim = runpy.run_path(str(SOURCE))
rerank = shim['jev_rerank']


class ChoiceAdapter(unittest.TestCase):
    def payload(self):
        return {'messages': [{'role': 'user', 'content': json.dumps({
            'query': 'PRIVATE_SYNTHETIC_QUERY',
            'candidates': [
                {'candidate': 1, 'title': 'One', 'text': 'first fixture'},
                {'candidate': 2, 'title': 'Two', 'text': 'second fixture'},
            ],
        })}]}

    def call(self, probabilities):
        response = io.BytesIO(json.dumps({'answers': {'best': {
            'probabilities': probabilities,
        }}}).encode())
        with patch('urllib.request.urlopen', return_value=response):
            return rerank(self.payload())

    def test_complete_probabilities_keep_the_provider_order(self):
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(self.call({'c1': 0.2, 'c2': 0.8}), {'scores': [
                {'candidate': 1, 'relevance': 0.2},
                {'candidate': 2, 'relevance': 0.8},
            ]})

    def test_partial_probabilities_refuse_to_fabricate_a_score(self):
        for probabilities in [{}, {'c2': 1.0}]:
            with self.subTest(probabilities=probabilities), self.assertRaises(ValueError):
                self.call(probabilities)

    def test_invalid_probabilities_are_refused(self):
        for invalid in [None, '0.2', True, float('nan'), float('inf'), -0.1, 1.1]:
            with self.subTest(value=invalid), self.assertRaises(ValueError):
                self.call({'c1': invalid, 'c2': 0.8})

    def test_request_content_is_absent_from_success_logs(self):
        output = io.StringIO()
        with contextlib.redirect_stderr(output):
            self.call({'c1': 0.2, 'c2': 0.8})
        self.assertIn('jev-choice 2 candidates', output.getvalue())
        self.assertNotIn('PRIVATE_SYNTHETIC_QUERY', output.getvalue())
        self.assertNotIn('first fixture', output.getvalue())

    def test_provider_exception_content_is_absent_from_failure_logs(self):
        handler = object.__new__(shim['Handler'])
        payload = self.payload()
        payload['messages'].insert(0, {
            'role': 'system', 'content': shim['RERANK_SYSTEM_PREFIX'],
        })
        raw = json.dumps(payload).encode()
        handler.path = '/v1/chat/completions'
        handler.headers = {'Content-Length': str(len(raw))}
        handler.rfile = io.BytesIO(raw)
        handler._reply = Mock()
        output = io.StringIO()
        failure = Mock(side_effect=RuntimeError('PRIVATE_PROVIDER_BODY'))
        with patch.dict(handler.do_POST.__globals__, jev_rerank=failure), \
                contextlib.redirect_stderr(output):
            handler.do_POST()
        handler._reply.assert_called_once_with(500)
        self.assertIn('RuntimeError', output.getvalue())
        self.assertNotIn('PRIVATE_PROVIDER_BODY', output.getvalue())

    def test_proxy_transport_diagnostics_omit_private_content(self):
        handler = object.__new__(shim['Handler'])
        handler.path = '/v1/models?private=PRIVATE_SYNTHETIC_PATH'
        handler.command = 'GET'
        handler.headers = {}
        handler._reply = Mock()
        output = io.StringIO()
        with patch('urllib.request.urlopen', side_effect=RuntimeError('PRIVATE_PROVIDER_BODY')), \
                contextlib.redirect_stderr(output):
            handler._proxy(None)
        self.assertIn('RuntimeError', output.getvalue())
        self.assertNotIn('PRIVATE_PROVIDER_BODY', output.getvalue())
        self.assertNotIn('PRIVATE_SYNTHETIC_PATH', output.getvalue())
        handler._reply.assert_called_once_with(
            502, json.dumps({'error': 'upstream transport failure'}).encode(),
        )
        handler._reply.reset_mock()
        # An upstream HTTP response still passes through unchanged.
        error = shim['urllib'].error.HTTPError(
            'http://fixture.invalid', 429, 'fixture limit', {}, io.BytesIO(b'fixture response'),
        )
        with patch('urllib.request.urlopen', side_effect=error):
            handler._proxy(None)
        handler._reply.assert_called_once_with(429, b'fixture response')


if __name__ == '__main__':
    unittest.main()
