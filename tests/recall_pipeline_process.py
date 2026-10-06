#!/usr/bin/env python3
"""召回⑤：仅本地合成 HTTP，CLI 两阶段、MCP 在线与离线分发。"""
import argparse
import contextlib
import http.server
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import threading
from mcp_smoke import Client, Responses

OWNER = "recall-cli-fixture"
AT = 1791000000
TEXT = "我的灯是紫色。"

class Fixture(Responses):
    requests = []
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        type(self).requests.append(body)
        payload = json.loads(body["input"])
        assert "model" not in payload
        assert body["model"] == "pro-fixture"
        assert body["reasoning"]["effort"] == "minimal"
        name = body["text"]["format"]["name"]
        if name == "r1_v1":
            output = {"need":"judge", "current_evidence":"query_only", "type":"single",
                      "subject":{"entity_id":"self"}, "relation":"灯色", "conditions":[], "time":None,
                      "plan":{"op":"lookup", "filter":{"relation":"fact","time":None},"unit":None}}
        else:
            assert name == "r4_v4"
            output = {"verdict":"supported", "facts":[{
                "statement":TEXT, "evidence":[{"source":{"kind":"stored","source_id":self.source_id},"quote":{"text":TEXT}}],
                "record_ids":[],"polarity":"affirm","strength":None,"uncertainty":None,"conditions":[],"times":[],
                "current":"yes","applies":"yes","supersedes_record_ids":[]}],"compute":None}
        data = json.dumps({"status":"completed","usage":{"input_tokens":10,"output_tokens":10},
                           "output":[{"type":"message","content":[{"type":"output_text","text":json.dumps(output,ensure_ascii=False)}]}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type","application/json")
        self.send_header("Content-Length",str(len(data)))
        self.end_headers()
        self.wfile.write(data)

def cli(binary, db, config, root, env, tool, args, success=True):
    path = root / "args.json"
    path.write_text(json.dumps(args, ensure_ascii=False))
    result = subprocess.run([str(binary),"--db",str(db),"--owner",OWNER,"--config",str(config),
                             "--call",tool,"--arguments",str(path)],env=env,text=True,capture_output=True,timeout=35)
    assert (result.returncode == 0) == success, (result.stdout,result.stderr)
    return json.loads(result.stdout) if success else result.stderr

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary",type=Path,required=True)
    binary = parser.parse_args().binary
    with contextlib.ExitStack() as stack:
        root = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="recall-cli-")))
        server = http.server.ThreadingHTTPServer(("127.0.0.1",0),Fixture)
        threading.Thread(target=server.serve_forever,daemon=True).start()
        stack.callback(server.server_close); stack.callback(server.shutdown)
        env = {k:v for k,v in os.environ.items() if not k.startswith(("PMR_","OPENAI_","DEEPSEEK_"))}
        env.update(PMR_MODEL_NETWORK="offline",DEEPSEEK_API_KEY="fixture-key",DEEPSEEK_BASE_URL=f"http://127.0.0.1:{server.server_port}/v1")
        config = root / "config.json"
        config.write_text(json.dumps({"recall_pipeline":"recall_v3","enrichment_mode":"off",
            "recall_models":{"r1":"pro-fixture","r4":"pro-fixture","reasoning_effort":{"r1":"minimal","r4":"minimal"}},
            "model_budget":{"group_id":"recall-cli-http","max_calls":6}}))
        db = root / "memory.sqlite3"
        client = Client(binary,db,env,OWNER,config=config)
        try:
            saved = client.call("memory_remember",{"session_id":"http","turn_id":"saved","text":TEXT,"capture_source":True,"observed_at":AT})
            Fixture.source_id = saved["source"]["source_id"]
            args = {"query":"我的灯是什么颜色？","request_id":"online","at":AT}
            online_args = dict(args, request_id="mcp-online")
            online = client.call("memory_recall",online_args)
            assert online["recall"]["status"] == "answered", online
            assert client.call("memory_recall",online_args) == online
            assert len(Fixture.requests) == 2
            offline = client.call("memory_recall",dict(args,request_id="offline",offline=True))
            assert offline["recall"]["semantic_judgment"] == "skipped"
            assert "answer_contract" not in offline and "answer_contract" not in offline["recall"]
        finally:
            client.close()
        result = cli(binary,db,config,root,env,"memory_recall",args)
        assert result["version"] == "recall.v4" and result["status"] == "answered",result
        assert result["facts"][0]["statement"] == TEXT
        assert "answer_contract" not in result
        assert [r["text"]["format"]["name"] for r in Fixture.requests] == ["r1_v1","r4_v4"] * 2
        # 新 CLI 进程无完整保留结果，不以核验为名再次调用。
        failure = cli(binary,db,config,root,env,"memory_validate_answer",dict(query=args["query"],request_id="online",draft_answer=TEXT,source_ids=[Fixture.source_id]),success=False)
        assert "not_retained" in failure
        assert len(Fixture.requests) == 4
        with sqlite3.connect(db) as c:
            assert c.execute("SELECT count(*) FROM model_calls").fetchone()[0] == 4
        print("recall CLI R1/R4=2; MCP online R1/R4=2 + replay=0, offline=0; reopen validation=0 sends")
if __name__ == "__main__":
    main()
