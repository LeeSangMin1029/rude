# DLL Prefetch로 Windows subprocess 시작 속도 개선

## 문제
- mir-callgraph subprocess가 Windows에서 ~1.3초 (rustc_driver.dll ~100MB 로딩)
- MSYS2 bash에서는 ~130ms (런타임 DLL 캐싱)
- 목표: 0.5초 이내 증분 업데이트

## 해결: PrefetchVirtualMemory
Mozilla Firefox가 사용한 방법 (Bug #1538279):
1. `CreateFileMapping` + `SEC_IMAGE`로 DLL을 메모리 매핑
2. `PrefetchVirtualMemory`로 매핑된 페이지를 OS 캐시에 올림
3. 후속 `LoadLibrary`(subprocess 내)가 캐시 히트 → 빠름

## 구현 계획

### 1단계: prefetch 함수 구현
파일: `crates/rude-intel/src/mir_edges/runner.rs`
- `prefetch_dll(path: &Path)` — Windows 전용
- `CreateFileW` → `CreateFileMappingW(SEC_IMAGE)` → `MapViewOfFile` → `PrefetchVirtualMemory` → cleanup
- nightly sysroot의 `rustc_driver-*.dll`, `std-*.dll` 대상

### 2단계: subprocess spawn 전에 prefetch 호출
- `run_mir_direct`에서 `find_mir_callgraph_bin` 후, spawn 전에 prefetch
- 첫 호출만 실행 (OnceLock 캐시)
- prefetch 실패 시 무시 (fallback: 기존 방식)

### 3단계: 측정
- RUDE_PROFILE=1로 lib_wait 시간 비교
- 목표: 1,300ms → ~200ms

## 참고
- https://bugzilla.mozilla.org/show_bug.cgi?id=1538279
- https://github.com/rust-lang/rust-analyzer/issues/18753
- https://github.com/rust-lang/rust/issues/8859
- PrefetchVirtualMemory: Windows 8+

## 결과: 실패
- PrefetchVirtualMemory + SEC_IMAGE: 오히려 5.7초로 느려짐 (캐시 오염)
- std::fs::read (파일 캐시): 효과 없음 (PE 재배치가 병목, 파일 I/O 아님)
- SetDllDirectory: 효과 없음
- DLL 복사 (exe 옆): 효과 없음

## 결론
Windows CreateProcessW로 rustc_driver.dll 의존 바이너리 실행 시 ~1.3초는
DLL 로딩 + PE 재배치 + rustc 초기화 시간. OS 레벨 한계.
daemon 모드(mir-callgraph 상시 실행 + IPC)만이 해결책.
