"""AF255 is opt-in and never rewrites historical labels or item review decisions."""
import copy
import unittest

import compare
import ledger
import test_compare


class Af255Report(unittest.TestCase):
    def document(self):
        document = test_compare.EndToEndReport().document()
        question = 'person_sketch_pen/person_q4'
        document.update(af255={'arm':'A'}, selected_questions=[question])
        document['cases'][0]['steps'][0]['question_binding'] = {'stages':{'R4':{'request':{'input':{'evidence':{'records':[1]}}}}}, 'write_snapshot':{'records':[1]}}
        return document

    def test_selected_denominator_and_missing_are_separate(self):
        document = self.document()
        document['cases'] = []
        report = compare.run(document)
        self.assertEqual(report['absolute_summary']['questions'],1)
        self.assertEqual(report['absolute_summary']['unselected'],62)
        self.assertEqual(report['absolute_summary']['not_recorded'],1)
        self.assertEqual(report['af255']['review_summary'],{'missing_recording':1})
        self.assertEqual(report['af255']['review_queue'],[])

    def test_automatic_pass_requires_independent_question_review_and_binds_full_output(self):
        document = self.document()
        report = compare.run(document)
        queue = report['af255']['review_queue']
        self.assertEqual(len(queue),1)
        item = queue[0]
        self.assertEqual(item['state'],'pending_review')
        accepted={'entries':{item['key']:{'binding':item['binding'],'decision':'accepted','reviewer':'fixture','reason':'verified entire answer'}}}
        self.assertEqual(compare.run(document,accepted)['af255']['review_summary'],{'human_confirmed':1})
        changed = copy.deepcopy(document)
        changed['cases'][0]['steps'][0]['result']['extra_field'] = 'full output changed'
        self.assertEqual(compare.run(changed,accepted)['af255']['review_summary'],{'pending_review':1})
        changed = copy.deepcopy(document)
        changed['cases'][0]['steps'][0]['question_binding']['stages']['R4']['request']['input']['evidence']['records']=[2]
        self.assertEqual(compare.run(changed,accepted)['af255']['review_summary'],{'pending_review':1})

    def test_mechanical_and_semantic_failure_cannot_be_exempted(self):
        binding = {'output':'fixture'}
        pending = ledger.question_review(binding,{'entries':{}},mechanical_ok=True,automatic_ok=True)
        accepted = {'entries':{pending['key']:{'binding':binding,'decision':'accepted','reviewer':'fixture','reason':'verified'}}}
        self.assertEqual(ledger.question_review(binding,accepted,mechanical_ok=False,automatic_ok=True)['state'],'mechanical_failure')
        self.assertEqual(ledger.question_review(binding,accepted,mechanical_ok=True,automatic_ok=False)['state'],'automatic_failure')
        wrong=copy.deepcopy(accepted);wrong['entries'][pending['key']]['binding']={'output':'other'}
        with self.assertRaises(ValueError):ledger.question_review(binding,wrong,mechanical_ok=True,automatic_ok=True)

    def test_r4_latency_is_original_remote_time_and_usage_excludes_r1(self):
        document = self.document()
        document['cases'][0]['calls']=[{'operation':'recall:person_q4','stage':stage,'origin':'replay','sends':0,'elapsed_ms':1,'recorded_elapsed_ms':ms,'http_status':200,'response_status':'completed','usage':{'input_tokens':10,'output_tokens':5}}
                                     for stage,ms in [('R1',1000),('R4',12000)]]
        report=compare.run(document)['af255']
        self.assertEqual(report['r4_latency']['R4_regular']['p50_ms'],12000)
        self.assertEqual(report['r4_usage'],{'input_tokens':10,'output_tokens':5})
        self.assertAlmostEqual(report['r4_official_reference_cost_cny'],.000225)

    def test_label_change_invalidates_question_key(self):
        first=ledger.question_review_binding(name='c/q',question={'query':'q'},query_meta={},expect={'status':['answered']},actual_input={},final_output={})
        second=copy.deepcopy(first);second['label']['expect_common']['status']=['conflict']
        keys=[ledger.question_review(x,{'entries':{}},mechanical_ok=True,automatic_ok=True)['key'] for x in (first,second)]
        self.assertNotEqual(*keys)


if __name__ == '__main__':
    unittest.main()
