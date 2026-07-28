<!-- Translated from README.md at d1f1bdcc10e38894b0151c67f80990199305bf26. -->
<div align="center">

<h1>LeanToken</h1>

**AI 코딩 토큰 하나하나를 더 가치 있게**

코딩 에이전트를 위한 로컬 우선 코드 인텔리전스 도구입니다. CLI와 MCP 서버를 통해
코드를 검색하고, 구조를 살펴보고, 정확한 범위를 읽고, Git 기록을 탐색할 수 있습니다.

**언어:** [English](../../../README.md) · [简体中文](../zh-CN/README.md) · [日本語](../ja-JP/README.md) · 한국어

<img src="../../../assets/leantoken-hero-v3.jpg" alt="대규모 코드베이스를 AI 에이전트에게 필요한 파일과 코드로 좁혀 주는 LeanToken" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![npm downloads](https://img.shields.io/npm/dm/leantoken?logo=npm&label=downloads)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#라이선스)

[빠른 시작](#빠른-시작) · [LeanToken을 사용하는 이유](#leantoken을-사용하는-이유) · [도구](#사용할-수-있는-도구) · [CLI](#cli-사용법) · [작동 방식](#작동-방식) · [문서](#문서)

</div>

---

> **번역 안내:** [영문 README](../../../README.md)가 최신이자 가장 완전한 기준입니다.
> 이 페이지는 설치와 일상적인 사용에 필요한 핵심 내용을 다룹니다. 버전, 안전성,
> 고급 검색 동작에 관한 세부 사항은 영문 문서를 기준으로 합니다.

> **측정된 토큰 절감량:** 60회 통제 실험에서 LeanToken은 에이전트의 기본 도구보다
> 제한적인 저장소 탐색 시 모델 입력 토큰을 20.1%, 광범위한 탐색 시 37.6% 적게
> 사용했습니다. 자세한 내용은 [측정 방법](../../measurement.md)을 참고하세요.

## 빠른 시작

Claude Code, Cursor, OpenCode, Codex, Gemini CLI 또는 Antigravity에 LeanToken을
추가합니다.

```bash
npx leantoken setup
```

설정 마법사는 감지된 클라이언트를 표시하지만 기본적으로 아무것도 선택하지 않습니다.
LeanToken을 사용할 코딩 에이전트를 직접 선택할 수 있으며, 파일을 쓰기 전에 정확한
설정 경로와 MCP 실행 명령을 보여 주고 확인을 요청합니다. `npx` 기반 설정은 실행된
LeanToken의 정확한 버전을 고정하므로 클라이언트를 다시 시작해도 조용히 업그레이드되지
않습니다.

설정한 클라이언트를 다시 시작하거나 새로고침한 뒤 저장소에서 연결과 첫 검색을
확인합니다.

```bash
npx leantoken doctor
```

예를 들어 *편집하기 전에 요청 취소와 관련된 코드를 찾아 줘* 같은 넓은 작업을 요청해
보세요. LeanToken은 에이전트가 `leantoken.context`로 시작하도록 돕고, 편집, 빌드,
테스트에는 기존 도구를 그대로 사용할 수 있게 합니다.

LeanToken이 관측한 저장소별 토큰 사용량을 확인합니다.

```bash
npx leantoken savings
```

| 특징 | 설명 |
| --- | --- |
| **기본적으로 로컬 실행** | 소스는 로컬 데이터베이스에 인덱싱됩니다. LeanToken은 읽기 전용 탐색 및 검색 계층입니다. |
| **명시적인 토큰 예산** | 모든 응답에 토큰 한도가 있어 큰 파일이 요청 전체를 차지하지 않습니다. |
| **에이전트 워크플로에 최적화** | 전용 도구로 파일 탐색, 코드 검색, 구조 확인, 정확한 범위 읽기, 기록 추적, JSON 질의, 토큰 사용량 확인을 수행합니다. |

### 자동 설정 및 제거

마법사를 건너뛰고 클라이언트를 명시적으로 선택하거나 지원되는 모든 클라이언트를
설정합니다.

```bash
npx leantoken setup --claude --codex --yes
npx leantoken setup --all --yes
```

파일을 변경하지 않고 동일한 설정 계획을 미리 봅니다.

```bash
npx leantoken setup --codex --cursor --dry-run
```

LeanToken이 관리하는 통합을 제거합니다.

```bash
npx leantoken remove
```

## 일반적인 에이전트 워크플로

LeanToken은 저장소 전체를 한 번에 쏟아붓는 방식보다 작은 증거 순환으로 사용할 때 가장
효과적입니다.

1. **편집 전에 방향 잡기.** 불확실한 작업은 `context`와 `plan_only: true`로
   시작합니다. 순위가 매겨진 경로, 범위, 경고를 확인한 뒤 `plan_only: false`로
   선택된 소스만 가져옵니다.
2. **소스를 다시 보내지 않고 계속하기.** 다음 context 호출에 이전 `receipt_id`를
   전달하거나 반환된 조각 해시를 `known_hashes`로 전달합니다.
3. **관측된 실패 조사하기.** `investigation` 워크플로를 사용하고
   `workflow_evidence`에는 직접 관측한 실패, 경로, 심볼 또는 테스트 의도만 제공합니다.
4. **변경 검토하기.** `review` 워크플로에서 `base_revision`을 `BASE..HEAD`로,
   `strict_changed_paths`를 `true`로 설정합니다.

## LeanToken을 사용하는 이유

대부분의 에이전트는 넓게 검색한 다음 파일 전체를 읽습니다. LeanToken은 이 작업을
단계적으로 좁힙니다.

| 일반적인 저장소 탐색 | LeanToken 사용 |
| --- | --- |
| 넓은 디렉터리 목록 스캔 | 간결한 트리에서 관련 경로 찾기 |
| 구조를 찾기 위해 파일 전체 읽기 | 파일 전체를 불러오지 않고 정의와 import 확인 |
| 매 턴 같은 코드 다시 보내기 | 변경되지 않은 증거의 중복 전송 방지 |
| 큰 파일이 요청을 가득 채우게 두기 | 소스를 정확한 예산 안에 유지하고 응답 오버헤드를 별도로 보고 |
| 중요한 파일 추측하기 | 작업과 관련성이 높은 코드의 순위 산정 |

코딩 에이전트는 계속해서 편집, 명령, 테스트, 대화를 담당합니다. LeanToken은 그 작업에
필요한 코드를 찾아 반환합니다.

## 사용할 수 있는 도구

| 도구 | 용도 |
| --- | --- |
| `leantoken.context` | 넓은 작업의 기본 시작점입니다. 토큰 예산 안에서 순위가 매겨진 증거를 미리 보거나 가져옵니다. |
| `leantoken.search` | 텍스트, 정규식, 식별자, 심볼 또는 참조를 순위 기반으로 검색합니다. |
| `leantoken.files` | ignore 규칙을 반영하는 간결한 경로 탐색입니다. |
| `leantoken.outline` | 파일 전체를 읽지 않고 정의, 시그니처, import, 범위를 확인합니다. |
| `leantoken.read` | 하나의 정확한 심볼 또는 포함 행 범위를 읽습니다. |
| `leantoken.history` | 변경 불가능한 Git 리비전 사이에서 파싱된 심볼을 읽고, 일괄 비교하고, 추적합니다. |
| `leantoken.json` | 제한된 현재 JSON을 질의, 요약 또는 비교합니다. |
| `leantoken.savings` | 응답 통계, 해시 억제, 실패, 관측 한계를 보고합니다. |

## CLI 사용법

`npx`로 직접 실행합니다.

```bash
npx leantoken status
npx leantoken savings
npx leantoken doctor
npx leantoken --root /path/to/repo search handle_request
```

또는 전역 바이너리를 설치합니다.

```bash
npm install --global leantoken@latest

leantoken --root /path/to/repo index
leantoken --root /path/to/repo search handle_request --mode identifier --max-tokens 800
leantoken --root /path/to/repo context \
  --task "fix request cancellation during shutdown" \
  --budget 2000
```

## 설치 및 업데이트

npm 패키지에는 다음 플랫폼용 네이티브 바이너리가 포함됩니다.

- macOS ARM64 및 x64
- glibc Linux ARM64 및 x64
- Windows x64

musl Linux를 포함한 다른 대상은 소스에서 빌드해야 합니다. Rust 1.95 이상과 네이티브
C/C++ 툴체인을 설치한 뒤 다음 명령을 실행하세요.

```bash
cargo install --git https://github.com/morluto/leantoken
```

기존 클라이언트 통합을 명시적으로 업데이트합니다.

```bash
npx --yes leantoken@latest setup --refresh --yes
```

고정된 MCP 설정은 조용히 `@latest`로 이동하지 않습니다. 롤백, 캐시 관리, 버전 세부
사항은 [사용 가이드](../../usage.md)를 참고하세요.

## 작동 방식

```text
저장소
  │
  ▼
파일 탐색 ──► 코드 구조 추출 ──► 로컬 검색 인덱스
                                  │
                                  ▼
에이전트 요청 ──► 순위 / 정확한 검색 ──► 토큰 예산 안의 대상 코드
```

LeanToken은 소스를 한 번 인덱싱한 뒤 간결한 경로, 순위가 매겨진 일치 항목, 구조
개요, 정확한 소스 범위, 작업별 컨텍스트를 제공합니다. 여러 턴에 걸쳐 변경되지 않은
증거를 다시 보내는 것도 피합니다.

## 문서

| 가이드 | 내용 |
| --- | --- |
| [사용법 및 도구 참고서](../../usage.md) | 명령, MCP 도구, 요청 옵션, 예시 |
| [아키텍처 및 신뢰성](../../architecture.md) | 구성 요소, 데이터 흐름, 저장소, 실패 동작 |
| [로드맵](../../roadmap.md) | 현재 방향과 계획된 작업 |
| [개발 및 테스트](../../development.md) | 로컬 설정, 검증, 릴리스 워크플로 |
| [벤치마크 방법론](../../../benchmarks/README.md) | 토큰 효율 측정 및 해석 |
| [측정 도구](../../measurement.md) | 실험, 전송 비용, 프로파일링 도구 |

## 라이선스

다음 중 하나를 선택하여 사용할 수 있습니다.

- [Apache License, Version 2.0](../../../LICENSE-APACHE)
- [MIT License](../../../LICENSE-MIT)
