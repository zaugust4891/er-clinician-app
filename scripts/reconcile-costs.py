#!/usr/bin/env python3
"""Compare provider cost totals for the same isolated run/window; never read credentials.
Input: {"bedrock":{"ledger_micros":123,"invoice_micros":123},
        "deepgram":{"ledger_micros":123,"invoice_micros":123}}.
Usage: python3 scripts/reconcile-costs.py evidence.json
"""
import json, sys
from decimal import Decimal

def reconcile(report):
    results={}
    for provider in ('bedrock','deepgram'):
        row=report[provider]
        ledger,invoice=(Decimal(str(row[k])) for k in ('ledger_micros','invoice_micros'))
        if not ledger.is_finite() or not invoice.is_finite() or ledger<0 or invoice<=0:
            raise ValueError('A positive, finite invoice and nonnegative ledger are required')
        delta=abs(ledger-invoice)/invoice
        results[provider]={'relative_error':str(delta),'passed':delta<=Decimal('0.05')}
    return results
if __name__=='__main__':
    results=reconcile(json.load(open(sys.argv[1])))
    print(json.dumps(results,indent=2))
    sys.exit(0 if all(r['passed'] for r in results.values()) else 1)
