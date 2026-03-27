# rustc_public API 한계 및 HIR 접근 조사 (2026-03-27)

## rustc_public (구 stable_mir) API 범위

nightly 1.96.0 기준. rustc_public은 MIR 수준 API만 제공한다.

### CrateItem::kind() — 4개 variant만 존재

```rust
pub enum ItemKind {
    Fn,
    Static,
    Const,
    Ctor(CtorKind),
}
```

Use, Mod, Trait, Impl, Struct, Enum 등은 없음.

### all_local_items() 반환 대상

MIR가 생성되는 아이템만:
- 함수 (일반, 메서드, 클로저)
- Static, Const
- 생성자 (struct/enum variant)

**미포함**: `use` 문, `mod` 선언, `type` alias, `trait` 선언, `impl` 블록, `extern` 블록, macro

### Crate 메서드

| 메서드 | 반환 |
|--------|------|
| fn_defs() | Vec<FnDef> |
| trait_decls() | TraitDecls |
| trait_impls() | ImplTraitDecls |
| statics() | Vec<StaticDef> |
| foreign_modules() | Vec<ForeignModuleDef> |

### CrateItem 메서드

kind, body, expect_body, has_body, ty, span, is_foreign_item,
requires_monomorphization, emit_mir, name, trimmed_name, def_id,
krate, tool_attrs, all_tool_attrs

## `use` 문 접근 방법

### 왜 rustc_public에 없는가

`use` 문은 name resolution 단계에서 처리되어 MIR에는 존재하지 않는다.
fully qualified path로 이미 해석된 상태이므로 MIR 수준에서는 import 정보가 불필요.

### 대안 1: rustc_hir + TyCtxt (unstable)

mir-callgraph에 이미 `extern crate rustc_middle`이 있으므로 접근 가능.
`rustc_public::run!` 콜백 안에서 `tcx`를 얻는 우회 필요.

```rust
// rustc_hir의 ItemKind::Use로 접근
for item_id in tcx.hir_crate_items(()).free_items() {
    let item = tcx.hir_item(item_id);
    if let rustc_hir::ItemKind::Use(path, kind) = &item.kind {
        // path: 해석된 경로
        // kind: UseKind::Single / Glob / ListStem
    }
}
```

장점: 100% 정확한 resolution 정보
단점: unstable API, nightly 변경에 취약

### 대안 2: syn 기반 텍스트 파싱

소스 파일을 syn으로 파싱하여 `use` 문 추출.
MIR의 `calls` 필드와 교차 검증으로 unused import 판별.

장점: rustc 내부 API 의존 없음, stable
단점: resolution 정보 없음 (어떤 crate의 어떤 타입인지 추론 필요)

### 대안 3: 하이브리드

syn으로 `use` 문 파싱 + MIR `calls`/`type_refs`로 참조 여부 검증.
resolution은 crate path prefix 매칭으로 근사.

## 현재 mir-callgraph의 타입 정보 추출 방식

struct/enum은 rustc_public의 ADT API로 직접 추출하지만,
`use` 문은 동일한 경로가 없어 우회 필요.

현재 extract.rs에서:
- `collect_type_chunks()`: trait_decls, trait_impls, ADT를 수집
- `extract_all()`: fn_defs에서 함수 + call edges 추출
- `fill_chunk_calls()`: calls 필드에 callee@line 형식으로 저장

calls 필드 예시:
```
commands::intel::query::load_or_build_graph@14, ops::Try::branch@14, ...
```

이 정보로 "어떤 함수가 어떤 경로를 호출하는가"는 알 수 있지만,
소스의 `use anyhow::Result` 같은 타입 import는 추적 불가.
