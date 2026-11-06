import importlib.util, json, pathlib, unittest, sys, re
sys.dont_write_bytecode = True
ROOT=pathlib.Path(__file__).resolve().parents[1]
spec=importlib.util.spec_from_file_location('costs',ROOT/'scripts/reconcile-costs.py')
costs=importlib.util.module_from_spec(spec); spec.loader.exec_module(costs)
class Reconciliation(unittest.TestCase):
    def test_boundary_and_missing_invoice(self):
        self.assertTrue(costs.reconcile({p:{'ledger_micros':105,'invoice_micros':100} for p in ('bedrock','deepgram')})['bedrock']['passed'])
        self.assertFalse(costs.reconcile({p:{'ledger_micros':106,'invoice_micros':100} for p in ('bedrock','deepgram')})['deepgram']['passed'])
        for bad in (0,-1,'NaN'):
            with self.assertRaises(ValueError): costs.reconcile({p:{'ledger_micros':1,'invoice_micros':bad} for p in ('bedrock','deepgram')})
    def test_published_methods_match_router(self):
        source=(ROOT/'crates/er-server/src/routes.rs').read_text()
        actual={}
        for match in re.finditer(r'\.route\(\s*"([^"]+)"\s*,',source):
            start=match.end(); depth=1; end=start
            while depth:
                if source[end]=='(':depth+=1
                elif source[end]==')':depth-=1
                end+=1
            actual[match[1]]=set(re.findall(r'\b(get|post|put|delete|patch)\(',source[start:end]))
        doc=json.loads((ROOT/'docs/api/openapi.json').read_text())
        published={path:set(methods) for path,methods in doc['paths'].items()}
        self.assertEqual(published,actual)
    def test_schema_refs_resolve(self):
        doc=json.loads((ROOT/'docs/api/openapi.json').read_text())
        def walk(value):
            if isinstance(value,dict):
                if '$ref' in value:
                    target=doc
                    for key in value['$ref'].removeprefix('#/').split('/'): target=target[key]
                for child in value.values():walk(child)
            elif isinstance(value,list):
                for child in value:walk(child)
        walk(doc)
        self.assertEqual(len(doc['paths']),20)
if __name__=='__main__': unittest.main()
