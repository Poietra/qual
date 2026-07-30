# JSON schemas

The checked-in schemas are the machine-readable source of truth. RFCs explain
semantics and invariants; schemas define accepted serialized structure.

## Diagnostics and operational output

- [`diagnostics-v1.json`](https://github.com/Poietra/qual/blob/main/schemas/diagnostics-v1.json)
- [`baseline-v1.json`](https://github.com/Poietra/qual/blob/main/schemas/baseline-v1.json)

## Semantic toolchain

- [`static-facts-v0.json`](https://github.com/Poietra/qual/blob/main/schemas/static-facts-v0.json)
- [`change-impact-v0.json`](https://github.com/Poietra/qual/blob/main/schemas/change-impact-v0.json)
- [`source-bridge-request-v0.json`](https://github.com/Poietra/qual/blob/main/schemas/source-bridge-request-v0.json)
- [`source-bridge-v0.json`](https://github.com/Poietra/qual/blob/main/schemas/source-bridge-v0.json)

## Validation

Schemas use JSON Schema Draft 2020-12. Consumers should reject documents with
an unsupported `schema_version` rather than trying to infer compatibility.

Example with Python's `jsonschema` package:

```python
import json
from pathlib import Path

from jsonschema import Draft202012Validator

schema = json.loads(Path("schemas/diagnostics-v1.json").read_text())
document = json.loads(Path("diagnostics.json").read_text())
Draft202012Validator(schema).validate(document)
```

Qual's own tests validate representative producer output against these
schemas and require deterministic serialization for identical inputs.
