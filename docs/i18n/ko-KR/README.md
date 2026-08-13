<!-- 루트 README와 함께 갱신합니다. 기술 사양의 기준 문서는 영어판입니다. -->
<div align="center">

# LeanToken

**명시적인 소스 토큰 예산을 제공하는 에이전트용 코드 인텔리전스.**

`mcp-name: io.github.morluto/leantoken`

**언어:** [English](../../../README.md) · [简体中文](../zh-CN/README.md) · [日本語](../ja-JP/README.md) · 한국어

<img src="../../../assets/leantoken-hero-v3.jpg" alt="저장소를 에이전트가 읽어야 할 코드로 좁히는 LeanToken" width="100%">

[![npm](https://img.shields.io/npm/v/leantoken?logo=npm&label=npm)](https://www.npmjs.com/package/leantoken)
[![Rust 1.95+](https://img.shields.io/badge/Rust-1.95%2B-000000?logo=rust)](https://www.rust-lang.org/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#라이선스)

</div>

LeanToken은 로컬에서 불변 저장소 generation을 만들고, 같은 generation에서 제한된 검색,
아웃라인, 읽기 작업을 제공합니다. CLI와 MCP 서버는 같은 애플리케이션 서비스를 호출하며
소스 코드는 로컬에 남습니다.

영문 README와 그 문서 링크가 완전한 기술 사양의 기준입니다. 이 페이지는 일상적인 사용에 필요한
현재 내용을 한국어로 정리합니다.

## 빠른 시작

지원되는 코딩 클라이언트에 LeanToken을 설정합니다.

```bash
npx leantoken setup
```

클라이언트를 재시작한 뒤 저장소에서 연결을 확인합니다.

```bash
npx leantoken doctor
```

CLI를 직접 사용할 수도 있습니다.

```bash
npx leantoken refresh
npx leantoken search RepositoryGeneration
npx leantoken outline src/storage/snapshot.rs
npx leantoken read src/storage/snapshot.rs --lines 1:120
```

`--allow-broad-root`를 명시하지 않으면 LeanToken은 파일 시스템 루트, 홈 디렉터리 및 홈
디렉터리의 상위를 거부합니다. 설정은 바꿀 파일을 미리 보여 주고 확인을 요구하며, 자동화에서는
클라이언트를 명시적으로 선택해야 합니다.

## 검색 모델

`refresh`가 공개 경계입니다.

```text
저장소 파일 -> 제한된 수집 -> 완전한 파생 generation
            -> 원자적 공개 -> search / outline / read
```

한 번의 검색은 하나의 커밋된 generation만 관찰하며, 서로 다른 공개의 파일을 섞지 않습니다.
워처와 호환성 조정 모드는 새로 고침을 요청할 수 있지만, 정확성은 파일 시스템 이벤트와의 경쟁에
의존하지 않습니다. 작업 트리를 바꾼 뒤에는 `refresh`를 호출하세요.

공개 `read`는 인덱싱된 generation의 내용을 반환합니다. 커밋되지 않은 작업 트리 바이트를
의도적으로 읽는 라이브러리 호출자는 약한 보장을 이해한 상태에서 명시적으로
`Services::read_worktree`를 사용합니다.

MCP 프로세스 하나는 저장소 루트 하나를 담당합니다. 여러 저장소에는 여러 프로세스를 시작하여
리소스와 장애를 분명하게 격리하세요.

## 사용 가능한 도구

| 도구 | 용도 |
| --- | --- |
| `leantoken.files` | 소스를 읽지 않고 인덱싱된 경로를 찾습니다. |
| `leantoken.search` | 텍스트, 정규식, 식별자, 심볼, 참조를 검색합니다. |
| `leantoken.outline` | 정의, 시그니처, import, 범위를 살펴봅니다. |
| `leantoken.read` | 인덱싱된 정확한 범위, 심볼, 제목을 읽습니다. |
| `leantoken.history` | 제한된 Git 기록과 diff를 조사합니다. |
| `leantoken.json` | 제한된 라이브 JSON 구조를 질의합니다. |
| `leantoken.context` | 작업에 대한 순위가 매겨진 증거를 구성합니다. |
| `leantoken.receipt_rebase` | 불변 증거를 새 generation으로 리베이스합니다. |
| `leantoken.savings` | 검색과 토큰의 관측값을 보고합니다. |

`files`, `search`, `outline`, `read`가 검색 커널입니다. `context`는 이 기본 연산을
조정합니다. Git, JSON, 설정, 업데이트, 캐시 관리, 오프라인 분석은 각각 별도 소유자가
있습니다.

## 토큰, 커서, 수명 주기

소스 예산과 직렬화된 응답 예산은 분리됩니다. `max_tokens`는 반환 소스를,
`max_response_tokens`는 전체 JSON 응답을 제한합니다. 페이지 커서는 저장소, generation,
정규화된 요청, 위치에 묶이므로 다른 내용이나 인수로 조용히 이어질 수 없습니다. 클라이언트는
이미 가진 콘텐츠 해시를 전달해 재전송을 피할 수 있습니다. 검색 증거와 쿼리 증명은 가변 세션이
아닌 불변의 콘텐츠 주소화 아티팩트입니다.

설정, 캐시, 사설 런타임 정리는 `--dry-run`으로 미리 볼 수 있습니다. 관리되는 클라이언트
설정은 선택한 버전에 고정되며 자동으로 업데이트되지 않습니다.

```bash
npx leantoken setup --claude --codex --yes
npx --yes leantoken@latest setup --refresh --yes
```

## 문서

| 문서 | 내용 |
| --- | --- |
| [사용법](../../usage.md) | 현재 CLI 및 MCP 동작 |
| [아키텍처](../../architecture.md) | generation, 저장소, 제한 계약 |
| [개발](../../development.md) | 테스트 소유권과 기여 절차 |
| [벤치마크](../../../benchmarks/README.md) | 선택적 평가 도구와 증거 정책 |
| [릴리스](../../releases.md) | 배포와 복구 절차 |
| [변경 로그](../../../CHANGELOG.md) | 릴리스된 변경 |

계획은 GitHub 이슈와 pull request에서 관리합니다. 과거 설계와 실험 서술은 최신 문서로
유지하지 않고 Git 기록에 남깁니다.

## 라이선스

MIT License 또는 Apache License 2.0 중 하나를 선택해 사용할 수 있습니다.
