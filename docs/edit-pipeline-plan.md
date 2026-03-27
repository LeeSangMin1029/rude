# rude 편집 파이프라인 현황 및 계획

## 파이프라인

```
extract_all()
├── Phase 1: HIR (단일 패스, free_items)
│   ├── 모든 아이템 span → span_map
│   ├── Use items (ListStem skip, Single만) → mir_uses
│   └── 함수 body DefId walk → mir_use_deps (DefId 불일치로 미완)
│
└── Phase 2: MIR
    ├── Type chunks + HIR span 즉시 적용
    ├── Function chunks + call edges
    └── fill_chunk_calls
```

## 완료

- [x] rude-util 크레이트 생성 (path, hash, scan, format, interrupt)
- [x] rude-db → 순수 DB 크레이트
- [x] HIR span 추출 → struct/enum 전체 범위
- [x] ListStem skip → 개별 Single 처리
- [x] mir_uses 정확한 resolved path
- [x] splice sort_key 수정
- [x] 1-based line 통일
- [x] resolve() trait impl 패턴 매칭
- [x] file_hint 양방향 경로 매칭

## 남은 작업

### 1. mir_use_deps DefId 매칭 문제
import의 DefId(예: anyhow::Result = type alias)와 사용처의 DefId(예: core::result::Result)가 다를 수 있음. 별도 resolution layer 필요하거나 텍스트 기반 보완.

### 2. clean-imports mir_uses 기반 전환
mir_uses의 resolved path에서 leaf ident 추출 → 파일 내 사용 여부 텍스트 검색. 100% 정확하진 않지만 실용적.

### 3. split import 자동화
이동 심볼의 body text에서 사용하는 ident → mir_uses에서 매칭 → 필요한 use만 새 파일에 복사.

### 4. canonicalize 누락 5곳 safe_canonicalize 적용

### 5. split re-export 삽입 위치 수정
현재 마지막 use 뒤에 삽입 — mod tests 안의 use super::*도 매칭. top-level only로 제한 필요 (indentation 체크).
