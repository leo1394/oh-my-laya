"""Authenticated HTTP date-cohort acceptance in an isolated database."""
from http.cookiejar import CookieJar
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
import urllib.error
import urllib.parse
import urllib.request

from test_service_failures import BINARY, Service


@unittest.skipUnless(BINARY.exists(), "Build the Rust service first")
class TimeFilterTests(unittest.TestCase):
    def test_overview_and_cases_share_decision_dates_before_pagination(self):
        with tempfile.TemporaryDirectory(prefix="laya-dates-", dir="/private/tmp") as directory:
            root = Path(directory)
            service = Service(root)
            service.close()
            with sqlite3.connect(root / 'laya.sqlite3') as db:
                for index, stamp in enumerate((200, 300, 400), 1):
                    identity = f'date-{index}'
                    db.execute("INSERT INTO decisions(id,request_id,request_json,recording_status,created_at,protected) VALUES(?,?,?,'stored',?,1)",
                               (identity, identity, '{"state":"date filter fixture"}', stamp))
                    db.execute("INSERT INTO reviews(decision_id,revision,status,labels_json,reason,actor_json,created_at) VALUES(?,1,'confirmed','{}','fixture','null',1000)", (identity,))
                    db.execute("INSERT INTO cases(id,decision_id,review_revision,content_json,labels_json,task_family,language,applicability,content_hash,created_at) VALUES(?,?,1,'{}','{}','general','en','task-fact',?,?)",
                               (f'case-{index}', identity, f'hash-{index}', 1000 + index))
            service = Service(root)
            try:
                service.consent()
                for index in (1, 2, 3):
                    receipt = service.rpc('feedback', {
                        'protocol_version': 1, 'event_id': f'date-usage-{index}',
                        'decision_id': f'date-{index}', 'attempt_ref': 'attempt', 'kind': 'usage',
                        'source': {'host': 'fixture', 'role': 'tester', 'actor_type': 'agent'},
                        'payload': {'total_tokens': index * 10, 'source': 'fixture', 'source_verified': True,
                                    'scope': 'attempt', 'checkpoint': 'fixture', 'overlap_status': 'non_overlapping'},
                    })
                    self.assertEqual(receipt['status'], 'stored')
                url = urllib.parse.urlparse(service.rpc('pair')['url'])
                origin = f'{url.scheme}://{url.netloc}'
                opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(CookieJar()))

                def request(path, body=None):
                    req = urllib.request.Request(origin + '/api/v1/' + path,
                        data=None if body is None else json.dumps(body).encode(),
                        headers={'Origin': origin, 'Content-Type': 'application/json'})
                    with opener.open(req, timeout=10) as response:
                        return json.loads(response.read())

                request('pair', {'code': urllib.parse.parse_qs(url.fragment)['pair'][0]})
                scope = 'created_after=200&created_before=300'
                overview = request('overview?' + scope)
                self.assertEqual(overview['dashboard']['tokens']['recorded_total'], 30)
                self.assertEqual(overview['dashboard']['learning']['reviewed_cases'], 2)
                self.assertEqual(overview['counts']['decisions'], 2)
                self.assertEqual(overview['counts']['feedback'], 2)
                self.assertEqual(sum(overview['risk_counts'].values()), 2)
                pages = [request(f'cases?{scope}&limit=1&offset={offset}')['items'] for offset in (0, 1, 2)]
                self.assertEqual({item['decision_id'] for page in pages for item in page}, {'date-1', 'date-2'})
                self.assertEqual(pages[2], [])
                self.assertEqual(request('status')['dashboard']['tokens']['recorded_total'], 60)
                self.assertEqual(request('cases?created_after=301&created_before=399')['items'], [])
                self.assertIsNone(request('overview?created_after=301&created_before=399')['dashboard']['tokens']['recorded_total'])
                for endpoint in ('overview', 'cases'):
                    for query in ('created_after=-1', 'created_before=no', 'created_after=300&created_before=200', 'unexpected=yes'):
                        with self.assertRaises(urllib.error.HTTPError) as error:
                            request(endpoint + '?' + query)
                        self.assertEqual(error.exception.code, 400)
                        error.exception.close()
            finally:
                service.close()


if __name__ == '__main__':
    unittest.main()
