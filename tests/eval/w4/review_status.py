"""Review status rule for generated W4 samples.

Review status is never generated: a sample keeps `reviewed` only when its
content (everything except label_status) is identical to the reviewed JSON
already on disk. Anything new or changed is `draft`.
"""


def status_for(new, old):
    """new: generated sample dict; old: dict loaded from disk, or None."""
    if old is None:
        return "draft"
    old = dict(old)
    prev = old.pop("label_status", None)
    cur = {k: v for k, v in new.items() if k != "label_status"}
    return "reviewed" if prev == "reviewed" and old == cur else "draft"
