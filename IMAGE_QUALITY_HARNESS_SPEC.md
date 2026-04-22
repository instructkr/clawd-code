# Image Quality Harness Spec

This document defines a concrete, harness-style specification for constrained image generation where creativity is preserved while deterministic quality gates enforce anatomy, symmetry, and pattern consistency.

## 1) Goals

- Keep **creative exploration** in composition, style, lighting, and storytelling.
- Enforce **hard constraints** for known failure classes:
  - hands/fingers,
  - feet/toes,
  - bilateral symmetry (where expected),
  - repeated pattern consistency (fabric prints, armor motifs, jewelry tiling, etc.).
- Operate as a **closed-loop controller**:
  1. generate,
  2. validate,
  3. target fixes,
  4. re-validate,
  5. accept/reject.

## 2) Non-goals

- Replacing the image model backend itself (SDXL/Flux/etc.).
- Global over-regularization that destroys artistic variance.
- "One prompt solves all" behavior.

## 2.1 Deployment/credential boundary

This spec is intentionally **image-pipeline only**. It does not depend on Claude/OpenAI chat APIs, auth token prompts, or LLM-provider specific credential flow.

- The harness orchestrates image tools and validators only.
- Any backend credential handling (if required by your deployment) should live in infrastructure/runtime config, not in tool schemas or prompt text.

## 3) Architecture

The harness is model-agnostic and treats image backends as pluggable providers.

```text
Prompt Plan -> Generate Image -> Validator Suite -> Score Aggregator
                                  |                     |
                                  v                     v
                           Region Failure Map      Policy Gate
                                  |                     |
                                  v                     |
                             Inpaint Planner <----------
                                  |
                                  v
                             Inpaint Region(s)
                                  |
                                  v
                            Re-score / Iterate
```

### 3.1 Components

- **Orchestrator**: state machine for staged generation + correction.
- **Tool adapters**: typed interfaces to generation/inpaint backends.
- **Validators**: detector + rule engines for anatomy/symmetry/patterns.
- **Policy engine**: weighted threshold gate for acceptance.
- **Benchmark runner**: deterministic regression scenes across seeds.

## 4) Typed Tool Schemas

These are the minimum interoperable contracts.

### 4.1 `generate_image`

Creates initial images with high creative latitude.

```json
{
  "name": "generate_image",
  "input_schema": {
    "type": "object",
    "required": ["prompt", "negative_prompt", "width", "height", "steps", "cfg", "seed", "sampler", "model"],
    "properties": {
      "prompt": { "type": "string" },
      "negative_prompt": { "type": "string" },
      "width": { "type": "integer", "minimum": 256, "maximum": 2048 },
      "height": { "type": "integer", "minimum": 256, "maximum": 2048 },
      "steps": { "type": "integer", "minimum": 8, "maximum": 150 },
      "cfg": { "type": "number", "minimum": 1.0, "maximum": 20.0 },
      "seed": { "type": "integer", "minimum": 0 },
      "sampler": { "type": "string" },
      "model": { "type": "string" },
      "style_preset": { "type": "string" },
      "batch_size": { "type": "integer", "minimum": 1, "maximum": 16 },
      "control_inputs": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["type", "uri", "strength"],
          "properties": {
            "type": { "type": "string", "enum": ["pose", "depth", "edge", "reference"] },
            "uri": { "type": "string" },
            "strength": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
          }
        }
      }
    }
  },
  "output_schema": {
    "type": "object",
    "required": ["images", "metadata"],
    "properties": {
      "images": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["uri", "seed"],
          "properties": {
            "uri": { "type": "string" },
            "seed": { "type": "integer" }
          }
        }
      },
      "metadata": { "type": "object" }
    }
  }
}
```

### 4.2 `detect_hands_feet`

Returns detections and structural quality signals.

```json
{
  "name": "detect_hands_feet",
  "input_schema": {
    "type": "object",
    "required": ["image_uri"],
    "properties": {
      "image_uri": { "type": "string" },
      "min_confidence": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.25 }
    }
  },
  "output_schema": {
    "type": "object",
    "required": ["regions", "scores"],
    "properties": {
      "regions": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["kind", "bbox", "confidence", "issues"],
          "properties": {
            "kind": { "type": "string", "enum": ["left_hand", "right_hand", "left_foot", "right_foot"] },
            "bbox": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            "confidence": { "type": "number" },
            "issues": {
              "type": "array",
              "items": {
                "type": "string",
                "enum": ["missing_digits", "extra_digits", "fused_digits", "unnatural_joint", "low_visibility"]
              }
            }
          }
        }
      },
      "scores": {
        "type": "object",
        "required": ["anatomy_score"],
        "properties": {
          "anatomy_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
        }
      }
    }
  }
}
```

### 4.3 `check_symmetry`

Validates bilateral/object symmetry when expected.

```json
{
  "name": "check_symmetry",
  "input_schema": {
    "type": "object",
    "required": ["image_uri", "expectations"],
    "properties": {
      "image_uri": { "type": "string" },
      "expectations": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["label", "left_region", "right_region"],
          "properties": {
            "label": { "type": "string" },
            "left_region": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            "right_region": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            "tolerance": { "type": "number", "minimum": 0.0, "maximum": 1.0, "default": 0.15 }
          }
        }
      }
    }
  },
  "output_schema": {
    "type": "object",
    "required": ["symmetry_score", "violations"],
    "properties": {
      "symmetry_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
      "violations": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["label", "delta", "left_bbox", "right_bbox"],
          "properties": {
            "label": { "type": "string" },
            "delta": { "type": "number" },
            "left_bbox": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            "right_bbox": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 }
          }
        }
      }
    }
  }
}
```

### 4.4 `check_pattern_consistency`

Detects broken repeats, motif drift, and seam artifacts.

```json
{
  "name": "check_pattern_consistency",
  "input_schema": {
    "type": "object",
    "required": ["image_uri", "pattern_regions"],
    "properties": {
      "image_uri": { "type": "string" },
      "pattern_regions": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["label", "bbox"],
          "properties": {
            "label": { "type": "string" },
            "bbox": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 }
          }
        }
      },
      "reference_region": {
        "type": "object",
        "properties": {
          "bbox": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 }
        }
      }
    }
  },
  "output_schema": {
    "type": "object",
    "required": ["pattern_score", "violations"],
    "properties": {
      "pattern_score": { "type": "number", "minimum": 0.0, "maximum": 1.0 },
      "violations": {
        "type": "array",
        "items": {
          "type": "object",
          "required": ["label", "error_type", "bbox", "severity"],
          "properties": {
            "label": { "type": "string" },
            "error_type": { "type": "string", "enum": ["scale_drift", "orientation_drift", "broken_repeat", "seam_break"] },
            "bbox": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 },
            "severity": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
          }
        }
      }
    }
  }
}
```

### 4.5 `inpaint_region`

Executes localized correction without resetting whole-scene creativity.

```json
{
  "name": "inpaint_region",
  "input_schema": {
    "type": "object",
    "required": ["image_uri", "mask_uri", "prompt", "negative_prompt", "denoise_strength", "steps", "cfg", "seed", "model"],
    "properties": {
      "image_uri": { "type": "string" },
      "mask_uri": { "type": "string" },
      "prompt": { "type": "string" },
      "negative_prompt": { "type": "string" },
      "denoise_strength": { "type": "number", "minimum": 0.05, "maximum": 0.85 },
      "steps": { "type": "integer", "minimum": 8, "maximum": 150 },
      "cfg": { "type": "number", "minimum": 1.0, "maximum": 20.0 },
      "seed": { "type": "integer", "minimum": 0 },
      "model": { "type": "string" },
      "preserve_edges": { "type": "boolean", "default": true }
    }
  },
  "output_schema": {
    "type": "object",
    "required": ["image_uri", "metadata"],
    "properties": {
      "image_uri": { "type": "string" },
      "metadata": { "type": "object" }
    }
  }
}
```

## 5) Quality Policy Gate

### 5.1 Score vector

Each candidate image receives a normalized score vector:

- `creative_score` (0..1) - optional aesthetic/intent matcher.
- `anatomy_score` (0..1) - from `detect_hands_feet` + optional face/body validator.
- `symmetry_score` (0..1) - from `check_symmetry`.
- `pattern_score` (0..1) - from `check_pattern_consistency`.
- `artifact_score` (0..1) - blur/noise/compression/edge artifacts.

### 5.2 Acceptance rule

Suggested strict policy for "production" mode:

```text
accept if:
  anatomy_score >= 0.92
  symmetry_score >= 0.90 (only when symmetry expectations are defined)
  pattern_score >= 0.88 (only when pattern regions are defined)
  artifact_score >= 0.85
  weighted_total >= 0.90
```

Recommended weighted total:

```text
weighted_total =
  0.40 * anatomy_score +
  0.25 * symmetry_score +
  0.20 * pattern_score +
  0.10 * artifact_score +
  0.05 * creative_score
```

## 6) Iterative Correction Loop

### 6.1 High-level loop

1. **Generate** `N` candidates (seed sweep, broad creativity settings).
2. **Validate** all candidates via detector suite.
3. **Rank** by weighted score + prompt intent similarity.
4. **Select** top `K` for targeted repair.
5. **Plan masks** from validator failure regions.
6. **Inpaint** only failing zones using constrained prompts.
7. **Re-validate** and compare deltas.
8. Stop on pass or max iterations.

### 6.2 Pseudocode

```python
MAX_ITERS = 4
TARGET_PASS = 0.90

candidates = generate_seed_sweep(scene_prompt, seeds)

for image in candidates:
    for i in range(MAX_ITERS):
        report = validate(image)
        if passes_policy(report):
            accept(image, report)
            break

        plan = build_inpaint_plan(report.violations)
        image = inpaint(image, plan)

    if not is_accepted(image):
        reject(image)
```

### 6.3 Repair prompt templates

Use narrowly scoped prompts per region class:

- **Hands/fingers:**
  - positive: "anatomically correct hand, five distinct fingers, natural knuckles, consistent skin texture"
  - negative: "extra digits, fused fingers, malformed thumb, broken anatomy"
- **Feet/toes:**
  - positive: "anatomically correct foot with natural toe separation and alignment"
  - negative: "extra toes, missing toes, fused toes, deformed foot"
- **Symmetry region:**
  - positive: "mirror-consistent design and stitching across both sides"
  - negative: "asymmetrical ornament, mismatched sleeve pattern"
- **Pattern region:**
  - positive: "continuous repeating motif with consistent scale and orientation"
  - negative: "broken repeat, warped pattern, seam discontinuity"

## 7) Operating Profiles

### 7.1 Exploration profile

- `generate_image`: higher seed variance, wider CFG range.
- Lower temporary pass gate (e.g., `weighted_total >= 0.82`).
- Purpose: discover high-potential compositions.

### 7.2 Production profile

- Reduced variance, tighter style lock.
- Strict pass gates from section 5.2.
- Maximum 2-4 correction cycles.

## 8) Regression Suite Specification

Benchmark with fixed scene packs + seeds to track quality drift.

### 8.1 Scene categories

1. single-person portrait with visible hands,
2. full-body pose with visible hands/feet,
3. symmetric garment (jacket/armor),
4. repeating textile pattern,
5. object array with repeated motifs.

### 8.2 Fixture format

```json
{
  "id": "scene_003_fullbody_armor",
  "prompt": "cinematic full body knight with ornate mirrored pauldrons...",
  "negative_prompt": "extra fingers, fused hands, asymmetrical armor, broken ornament",
  "width": 1024,
  "height": 1024,
  "model": "sdxl-base-1.0",
  "seeds": [101, 202, 303, 404],
  "expectations": {
    "requires_hands": true,
    "requires_feet": true,
    "symmetry_labels": ["pauldrons", "gauntlets"],
    "pattern_labels": ["chest_ornament"]
  }
}
```

### 8.3 Metrics and pass/fail

Track per commit/release:

- `pass_rate`: percentage of images accepted under strict gate.
- `avg_iterations_to_pass`: lower is better.
- `regional_fix_success_rate`: per violation class.
- `catastrophic_failure_rate`: unrecoverable anatomy/symmetry errors.

Recommended release gate:

```text
pass_rate >= 85%
catastrophic_failure_rate <= 2%
avg_iterations_to_pass <= 2.5
```

## 9) Suggested SDXL Defaults

These defaults preserve creativity in base generation while supporting precise repair:

- **Base generation (Stage A)**
  - steps: 28-40
  - cfg: 5.0-7.5
  - sampler: DPM++ 2M Karras (or equivalent)
  - seed sweep: 8-24 seeds per scene
- **Correction inpaint (Stage B)**
  - steps: 20-36
  - cfg: 5.5-8.0
  - denoise: 0.18-0.42 (region dependent)
  - mask padding: 8-24 px around violation bbox

## 10) Implementation Plan in This Harness Style

1. Add typed tool definitions for the five core operations.
2. Register adapters for selected provider(s):
   - local ComfyUI graph endpoint,
   - self-hosted Diffusers service,
   - internal image-render worker service.
3. Add validator execution stage and normalized score model.
4. Add policy gate module with profile support.
5. Add iterative repair planner (violation -> mask -> prompt template).
6. Add regression runner and CI summary output.

## 11) JSON Report Contract

Every run should emit a machine-readable audit report:

```json
{
  "run_id": "iqh_2026_04_22_001",
  "profile": "production",
  "scene_id": "scene_003_fullbody_armor",
  "result": "accepted",
  "scores": {
    "anatomy_score": 0.95,
    "symmetry_score": 0.93,
    "pattern_score": 0.91,
    "artifact_score": 0.89,
    "creative_score": 0.84,
    "weighted_total": 0.92
  },
  "iterations": 2,
  "violations_resolved": ["extra_digits", "orientation_drift"],
  "artifacts": {
    "final_image_uri": "s3://.../final.png",
    "history": [".../iter0.png", ".../iter1.png", ".../iter2.png"]
  }
}
```

## 12) Why this works

- Preserves probabilistic discovery where it helps.
- Applies deterministic constraints only where needed.
- Converts quality requirements into executable gates and measurable regressions.

This is the "secret sauce" pattern: **creative model + strict validator policy + iterative local repair**.
