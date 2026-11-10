#!/usr/bin/env python3
"""Adversarial protocol fixture only: no MTP, no local transfer side effects."""
import json
import struct
import sys


def send(value):
    payload = json.dumps(value).encode()
    sys.stdout.buffer.write(struct.pack('>I', len(payload)) + payload)
    sys.stdout.buffer.flush()


def read():
    header = sys.stdin.buffer.read(4)
    if len(header) != 4:
        return None
    return json.loads(sys.stdin.buffer.read(struct.unpack('>I', header)[0]))


send({'kind': 'hello', 'hello': {'protocol_version': 1, 'backend': 'kalam',
      'backend_version': 'malicious-protocol-fixture', 'build_id': 'test-only',
      'capabilities': {'simulated': False, 'browse': True, 'upload': True,
                       'download': True, 'cancel_current': False}}})
while (command := read()) is not None:
    operation = command['operation']['kind']
    if operation == 'connect':
        generation = 11
        value = {'kind': 'connected', 'device_key': 'protocol-fixture', 'display_name': 'No USB'}
    elif operation in ('upload_file', 'download_file'):
        # Correct request id, but success from a different raw worker session.
        generation = 12
        value = {'kind': 'transferred',
                 'direction': 'upload' if operation == 'upload_file' else 'download',
                 'actual_name': 'report.txt', 'bytes': 7, 'object_id': 42,
                 'local_path': list(b'/never-published/report.txt') if operation == 'download_file' else None,
                 'journal_path': list(b'/never-created/journal.json')}
    else:
        break
    send({'kind': 'result', 'request_id': command['request_id'], 'generation': generation,
          'outcome': {'status': 'success', 'value': value}})
