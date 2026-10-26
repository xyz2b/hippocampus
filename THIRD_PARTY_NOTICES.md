# Third-party notices

The project-owned code is licensed under MIT OR Apache-2.0. Third-party material
retains its own license; the project license does not replace upstream notices.

## LongMemEval (MIT)

- Upstream code and license: https://github.com/xiaowu0162/LongMemEval
- License text: https://github.com/xiaowu0162/LongMemEval/blob/main/LICENSE
- Original dataset: https://huggingface.co/datasets/xiaowu0162/longmemeval
- Cleaned dataset: https://huggingface.co/datasets/xiaowu0162/longmemeval-cleaned

`tests/eval/convert_longmemeval.py` accepts an externally supplied oracle JSON
(the documented filename is `longmemeval_oracle.json`). The exact input file,
upstream revision and download hash used for the checked-in resident cases were
not recorded（未记录）. Do not infer that they match a later cleaned release.
The converter retains USER turns, orders sessions by date, creates stable message
references, converts dates using UTC+8, and derives questions and expectations.

Separately, `examples/longmemeval_e1.py` pins `longmemeval_s_cleaned.json` to
revision `98d7416c24c778c2fee6e6f3006e7a073259d48f`, SHA-256
`d6f21ea9d60a0d56f34a05b609c79c88a451d2ae03597821ea3d5a9678c3a442`.
That pin describes the E1 preparation script, not the resident oracle conversion.

This notice covers LongMemEval-derived text in `tests/eval/cases/lme_*.json`,
related labels, fixed inputs, recordings, baseline/end-to-end results and scoring
reports under `tests/eval/`, and dataset-derived outputs of the LongMemEval
examples. Repository-authored synthetic cases are separate from that dataset.
No model output or transformed data is represented as a new upstream release.

The upstream MIT notice follows:

```text
MIT License

Copyright (c) 2024 Di Wu

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## Dependencies

Rust dependencies are pinned by `Cargo.lock` and retain their respective
copyright and license notices. This file is not a complete dependency license
audit; distributors of compiled binaries must also preserve applicable dependency
notices, including bundled native code.
