# sta AI 에이전트 (MCP) 안내

sta는 Claude Code, Claude Desktop, VS Code, Cursor 같은
[MCP(Model Context Protocol)](https://modelcontextprotocol.io/) 클라이언트가 브라우저를 사용할 수 있게
해 주는 MCP 서버 `sta-mcp.exe`(macOS에서는 `sta-mcp`)를 함께 제공합니다. 이 문서는 한국어 요약
안내이며, 모든 도구의 입력·출력·오류와 보안 모델 전체는 영어 참조 문서
[`docs/MCP.md`](MCP.md)에 있습니다.

```
MCP 클라이언트 ──stdio──► sta-mcp ──로컬 채널(현재 사용자 전용)──► sta
                                    Windows: named pipe
                                    macOS:   <데이터 폴더>/sta/agent.sock (권한 0600)
```

macOS에서는 `sta-mcp`가 앱 번들 안(`sta.app/Contents/MacOS/sta-mcp`)에 있고, 프로필은
`~/Library/Application Support/sta`(디버그 빌드는 `sta Dev`)입니다. 아래 경로 예시는 Windows
기준입니다.

- **기본값은 꺼짐**입니다. 설정에서 켜기 전에는 아무것도 대기하지 않습니다.
- 브라우저는 원격 디버깅 포트를 열지 않고, 자기 프로세스 안의 DevTools 연결로 페이지를 다룹니다.
- 새 클라이언트와 새 사이트는 **사용자가 승인**해야 하고, 에이전트는 기본적으로 **에이전트가 연 탭과
  사용자가 공유한 탭만** 볼 수 있습니다. 에이전트가 연 탭은 모든 에이전트가 함께 봅니다(새 세션이나
  동시에 연결된 다른 에이전트도 이전에 열린 탭을 볼 수 있음).

## 1. 설치와 연결

1. sta를 설치하거나 빌드합니다. `sta-mcp.exe`는 `sta.exe`와 같은 폴더에 있습니다
   (`cargo build -p sta -p sta-mcp` → `target\debug\`, 디버그 빌드의 프로필은
   `%LOCALAPPDATA%\sta Dev`). macOS에서는 `target/debug/sta-mcp`이고, 프로필은
   `~/Library/Application Support/sta Dev`입니다.
2. **설정 → AI agents (MCP) → Agent access**를 *Full*(전체) 또는 *Read only*(읽기 전용)로 바꿉니다.
3. 같은 화면의 **Connect a client**에서 쓰는 클라이언트를 고르고 설정을 복사해 등록합니다. 이
   설치본의 경로(기본이 아닌 프로필이면 `--data-dir`까지)가 들어간 설정이 표시됩니다. 아래 예시의
   `C:\Program Files\sta\`는 `sta.exe`가 있는 폴더를 뜻하는 자리 표시자입니다(소스
   빌드라면 예: `C:\src\sta\target\debug\`).
4. **Test connection**을 눌러 접근 켜짐 → sta 대기 중 → MCP 서버 발견 → MCP 서버가 sta에
   연결됨을 확인합니다(에이전트가 실제로 연결되지는 않으며 승인 창도 뜨지 않습니다).
5. 에이전트에게 브라우저 작업을 요청하면 처음 한 번 sta 창 오른쪽 위에 승인 창이 뜹니다.

### Claude Code

```bash
# macOS
claude mcp add sta -s user -- /Applications/sta.app/Contents/MacOS/sta-mcp
```

```powershell
# 모든 프로젝트에서 사용 (~/.claude.json)
claude mcp add sta -s user -- "C:\Program Files\sta\sta-mcp.exe"
# 현재 프로젝트에서 나만 사용 (기본 범위 -s local)
claude mcp add sta -- "C:\Program Files\sta\sta-mcp.exe"
# 프로젝트 팀과 공유 (프로젝트 루트의 .mcp.json)
claude mcp add sta -s project -- "C:\Program Files\sta\sta-mcp.exe"

claude mcp list      # sta … ✔ Connected (sta가 꺼져 있어도 도구 목록은 보입니다)
claude mcp remove sta -s user
```

`--` 뒤의 내용은 그대로 `sta-mcp.exe`에 전달됩니다(예: `... sta-mcp.exe" --data-dir C:\tmp\profile`).
`cmd /c` 같은 래퍼는 필요 없습니다. 단, **npm으로 설치한 Claude Code를 PowerShell에서** 실행하면
`claude.ps1`이 쓰이고 PowerShell이 `--`를 없애므로 `--data-dir`가 `claude`에 전달됩니다. 이때는
같은 인수로 `claude.cmd mcp add …`를 실행하세요(네이티브 설치의 `claude.exe`는 해당 없음).

### Claude Desktop

설정 → Developer → *Edit Config*로 `%APPDATA%\Claude\claude_desktop_config.json`을 열고 추가한 뒤
Claude Desktop을 다시 시작합니다. Microsoft Store(MSIX) 버전은
`%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\claude_desktop_config.json`
파일이며, 이 경우 Claude가 sta를 대신 실행할 수 없으므로 **sta를 먼저 열어 두세요**.

```json
{ "mcpServers": { "sta": { "command": "C:\\Program Files\\sta\\sta-mcp.exe" } } }
```

### VS Code / Cursor

- VS Code: 작업 영역의 `.vscode/mcp.json`
  `{ "servers": { "sta": { "type": "stdio", "command": "C:\\Program Files\\sta\\sta-mcp.exe" } } }`
- Cursor: `%USERPROFILE%\.cursor\mcp.json` 또는 프로젝트의 `.cursor\mcp.json`
  `{ "mcpServers": { "sta": { "command": "C:\\Program Files\\sta\\sta-mcp.exe" } } }`

### 명령줄 옵션

| 옵션 | 의미 |
|---|---|
| `--data-dir <폴더>` | sta 데이터 폴더(기본 `%LOCALAPPDATA%\sta`, 디버그 빌드는 `sta Dev`). 이름 변경 전 데이터 폴더를 아직 옮기지 못해 그 자리에서 쓰고 있는 sta도 찾습니다(README의 이름 변경 안내) |
| `--no-launch` | sta가 꺼져 있어도 실행하지 않고 `browser_not_running`으로 실패 |
| `--check` | MCP 없이 연결만 확인하고 JSON 한 줄을 출력(Test connection이 사용) |

sta가 꺼져 있고 접근이 켜진 상태로 저장되어 있으면, 첫 도구 호출 때 옆의 `sta.exe`를
콘솔 창 없이 실행하고 최대 25초 기다립니다(`--no-launch`나 MSIX 클라이언트에서는 실행하지 않음).

## 2. 설정과 승인 흐름

### 설정 (설정 → AI agents (MCP))

| 설정 | 값 (기본값 먼저) | 설명 |
|---|---|---|
| Agent access | 끄기 / 읽기 전용 / 전체 | 끄기: 연결 불가. 읽기 전용: 읽기 도구만(탭을 열거나 바꾸거나 클릭하지 않음). 전체: 모든 도구 |
| Tabs agents can see | 에이전트 탭 / 모든 탭 | 에이전트가 연 탭(어느 에이전트·세션이 열었든)과 사용자가 공유한 탭만, 또는 모든 탭 |
| Ask before a new site | 켜짐 | 에이전트가 처음 쓰는 사이트마다 사용자 확인 |
| Always allowed sites | 없음 | "항상"으로 허용한 사이트 |
| Blocked sites | 없음 | 에이전트가 절대 열거나 조작하지 못하는 사이트(하위 도메인 포함) |
| Devices on your network | 꺼짐 | 공유기·NAS·`.local` 같은 내부망 주소 허용 여부(localhost는 항상 허용). 꺼져 있으면 에이전트가 조작하는 탭의 모든 프레임에서 내부망 주소로의 이동이 취소됨(페이지가 직접 보내는 이미지·스크립트·`fetch` 요청은 거르지 않음) |
| Page scripts | 끄기 / Isolated / Page | `evaluate` 도구. Isolated는 페이지 스크립트와 분리된 공간, Page는 페이지 자체 공간에서 실행. Isolated라도 스크립트가 DOM에 `<script>`를 넣어 페이지 공간에서 코드를 실행할 수 있으므로(페이지 CSP가 막지 않는 한) 둘 다 "에이전트가 페이지에서 코드를 실행할 수 있음"으로 보세요 |
| Browsing history | 꺼짐 | `history_search` 허용 |
| Downloads list | 꺼짐 | `downloads_list` 허용(파일 이름과 상태만, 경로는 제외) |
| Trusted clients | 없음 | "항상 허용"한 서명된 프로그램. Revoke로 제거 |

### 승인 흐름

1. **클라이언트 연결 승인**: 클라이언트가 스스로 밝힌 이름·버전, MCP 서버를 실행한 프로그램
   (예: `claude.exe`, `node.exe`)과 서명자(*Verified*/*Unverified*), 받게 될 접근 수준이 표시됩니다.
   *Deny*(거부) / *Allow for this session*(이번 세션만) / *Always allow*(항상, 서명된 프로그램만).
2. **사이트 승인**: 에이전트가 새 사이트를 열거나 조작하려 하면 "Allow … on example.com?" —
   *Deny* / *This session* / *Always*.
3. **탭 공유 요청**: 에이전트가 `request_tab_access`로 탭(기본값은 사용자가 보고 있는 탭)을 요청하면
   탭 제목과 **에이전트가 직접 쓴 이유**가 표시됩니다 — *Deny* / *Share tab*(공유하면 그 탭의 현재
   사이트도 이번 세션 동안 허용됨). 사이드바에서 탭을 오른쪽 클릭 → *Share with AI Agents*로 직접
   공유하거나 해제할 수도 있습니다.

- 버튼은 창이 뜬 뒤 1초, 그리고 키를 누를 때마다 1초 동안 눌리지 않으며 기본(Enter) 버튼이 없습니다.
  Esc는 거부입니다. 사용자가 sta에서 타이핑 중이면 승인 창이 포커스를 가져가지 않습니다.
- 호출은 최대 20초 동안 답을 기다리고(이후 `not_approved`), 승인 창은 2분 뒤 자동으로 거부됩니다.
  연결을 거부하면 60초 동안 새 연결 승인 창이 뜨지 않습니다.
- 승인은 승인 창과 설정 페이지에서만 받습니다(다른 페이지에서는 403).

### 에이전트 확인과 중지

- **상단 바 칩**: 연결된 에이전트 이름(동작 중 깜빡임), 승인 대기, 일시 중지 상태. 누르면 최근 동작
  5개(실패 이유 포함), 대기 중인 다운로드(*Keep*/*Discard*), *Archive N agent tabs*가 보입니다.
- **주황색 2px 테두리**: 에이전트가 조작 중인 탭. 사이드바의 ✦는 에이전트 범위의 탭입니다.
- **직접 입력하면 탭을 돌려받습니다**: 그 탭에 사용자가 키를 입력하면 테두리와 보호 장치가 해제되고
  에이전트의 입력 도구는 2초간 `user_active`를 받습니다.
- **Stop**: 모든 에이전트를 끊고 *Resume*할 때까지 일시 중지합니다. 재개 후에는 클라이언트에서 MCP
  서버를 다시 시작해야 합니다.
- 기록: `<데이터 폴더>\Logs\agent.log`(도구, 탭, 사이트, 소요 시간, 결과만 기록하며 페이지 내용·입력한
  텍스트·스크립트·전체 URL은 기록하지 않음).

## 3. 도구 목록

"화면"은 탭이 화면에 보여야 하는 도구입니다(`tab_show`로 먼저 표시, 창 최소화 불가). 나머지는
백그라운드 탭에서도 동작합니다(`scroll`은 탭이 한 번이라도 화면에 표시된 뒤).

| 도구 | 접근 | 화면 | 설명 |
|---|---|---|---|
| `tabs_list` | 읽기 | | 사용할 수 있는 탭 목록 |
| `tab_open` | 전체 | | 새 백그라운드 탭에서 http(s) 주소 열기 |
| `tab_navigate` | 전체 | | 주소 이동, 뒤로, 앞으로, 새로고침 |
| `tab_show` | 전체 | | 탭을 화면에 표시(키보드 포커스는 옮기지 않음) |
| `tab_close` | 전체 | | 에이전트가 연 탭 닫기(보관함으로 이동) |
| `request_tab_access` | 읽기 | | 사용자에게 탭 공유 요청(이유 표시) |
| `page_snapshot` | 읽기 | | 접근성 트리 개요와 요소 ref(`12.3.5`) |
| `page_text` | 읽기 | | 읽을 수 있는 텍스트(text/markdown, offset으로 페이지 나눔) |
| `page_find` | 읽기 | | 텍스트나 정규식 검색, 결과 위치는 `page_text` offset |
| `page_screenshot` | 읽기 | ✓ | 화면, 요소, 또는 전체 페이지(`fullPage`, 에이전트가 연 탭만) 이미지 |
| `click` | 전체 | ✓ | ref나 좌표 클릭(가려진 요소는 `element_obscured`) |
| `hover` | 전체 | ✓ | 마우스를 요소 위로 이동(메뉴·툴팁) |
| `type` | 전체 | | 텍스트 입력(`clear`, `submit`, `slowly`) |
| `press_key` | 전체 | | 키나 조합(`Enter`, `Control+A`) 입력 |
| `select_option` | 전체 | | `<select>` 옵션 선택(값 또는 라벨) |
| `scroll` | 전체 | | 요소를 화면에 보이게 하거나 페이지·요소 스크롤 |
| `fill_form` | 전체 | | 여러 입력란(텍스트, 선택, 체크박스, 라디오, 날짜) 한 번에 채우기 |
| `wait_for` | 읽기 | | 텍스트, 선택자, URL, 로드 상태, 시간 대기 |
| `handle_dialog` | 전체 | | alert/confirm/prompt 응답 |
| `evaluate` | 전체 + Page scripts | | JavaScript 함수 실행(기본은 격리된 공간) |
| `console_messages` | 읽기 | | 최근 콘솔 메시지(탭당 최대 500개, 접근이 켜진 동안만 메모리에 보관) |
| `history_search` | 읽기 + Browsing history | | 방문 기록 검색(에이전트가 열 수 없는 페이지 제외) |
| `downloads_list` | 읽기 + Downloads list | | 다운로드 목록(파일 이름과 상태만) |

기본 사용 흐름: `tab_open` → `page_snapshot` → ref로 `click`·`type`·`fill_form` → `page_text`·
`page_find`로 결과 확인. 페이지가 바뀌면 이전 ref는 `stale_ref`가 되므로 스냅샷을 다시 찍습니다.
오류는 `Error [코드]: 메시지. Hint: 해결 방법` 형식의 도구 결과로 돌아옵니다(코드 목록은
[`MCP.md` §7](MCP.md#7-errors)). 모델에 필요한 내용은 모두 결과의 텍스트(스크린샷은 이미지)에 있고,
프로그램용 구조화 데이터(id·개수·플래그)는 `structuredContent`가 아니라 `_meta`의
`"sta/structured"`로 보냅니다(Claude Code는 `structuredContent`가 있으면 그것만 모델에 보여 줌).
아직 그려지지 않은 새 페이지는 처음 약 0.5초 동안 키 입력을 무시하므로, 키를 누르는 도구는 이동 직후
최대 약 0.75초 기다립니다.

## 4. 보안 주의사항

- **승인한 에이전트는 사용자로서 동작합니다.** 로그인된 계정과 허용한 사이트에서 읽고, 클릭하고,
  입력합니다. 에이전트가 읽은 페이지 내용·스크린샷·입력란 값·스크립트 결과는 클라이언트의 AI
  제공자에게 전송됩니다.
- **프롬프트 인젝션**: 웹 페이지에는 AI를 속이려는 문장이 있을 수 있습니다. sta는 페이지에서 온
  모든 문자열을 매 호출마다 새로운 무작위 표식으로 감싸 "데이터일 뿐 지시가 아님"을 알리고, 페이지
  내용이 접근 수준·범위·허용 사이트·설정을 바꾸지 못하게 하지만, 모델이 속지 않는다고 보장할 수는
  없습니다.
  - *Ask before a new site*를 켜 두세요.
  - 은행·메일·관리 콘솔 같은 민감한 사이트는 **Blocked sites**에 추가하세요.
  - 조사·요약 작업은 **읽기 전용**을 쓰세요.
  - **Page scripts**는 필요할 때만 켜세요(스크립트는 사용자가 입력한 비밀번호를 포함해 페이지가 볼 수
    있는 모든 것을 읽을 수 있습니다).
  - 예상하지 못한 사이트·탭 승인 요청은 거부하고, 이상한 동작을 보면 바로 **Stop**을 누르세요.
- **에이전트가 조작 중인 탭의 보호 장치**: 외부 프로그램 링크(`mailto:` 등) 실행 차단, 다운로드는
  사용자가 *Keep*/*Discard* 결정, 전체 화면 거부, 파일 선택 창 취소(파일 업로드 불가), 권한 요청(카메라,
  위치, 알림 등) 거절, JavaScript 대화상자는 `handle_dialog`로만 응답, Peek 대신 백그라운드 탭,
  차단·내부망·미승인 사이트로의 이동 취소, Chrome 웹 스토어 차단(에이전트는 확장 프로그램을 설치할 수 없음).
- **"항상 허용"은 신원 확인이 아니라 동의입니다.** 실행 파일 경로와 서명자로 기억합니다. npm으로 설치한
  Claude Code는 `node.exe`(OpenJS Foundation 서명)로 실행되므로, 이를 신뢰하면 Node 기반의 다른 MCP
  클라이언트도 신뢰하게 됩니다.
- 같은 사용자 권한으로 실행되는 악성 프로그램은 막을 수 없습니다(다른 사용자, 낮은 무결성
  수준·AppContainer 프로세스, 웹 페이지는 채널에 접근할 수 없음). MCP 서버는 연결 전에 파이프의
  소유자·세션·무결성 수준(medium 이상)·서버 프로세스를 확인하므로, 낮은 무결성 프로세스가 파이프 이름을
  가로채 브라우저인 척할 수도 없습니다(`endpoint_untrusted`).
- macOS에서는 소켓이 사용자 데이터 폴더 안에 권한 0600으로 만들어지고, 브라우저는 uid가 다른
  클라이언트를 끊습니다. MCP 서버도 쓰기 전에 커널에서 상대의 uid와 pid를 직접 확인해
  엔드포인트 파일이 가리키는 프로세스가 맞는지 검사합니다
  (`getsockopt(SOL_LOCAL, LOCAL_PEERCRED/LOCAL_PEERPID)`). 다만 서명자 확인이 없으므로
  **"항상 허용"은 macOS에서 제공되지 않습니다**.

## 4-1. 디버그 전용 테스트 도구 (개발자용)

sta의 end-to-end 테스트는 브라우저를 **MCP로** 조작합니다. 이를 위해 `--features test-hooks`로
빌드한 **디버그 빌드에만** 존재하는 `test_*` 도구 35개가 따로 있습니다. 이 도구들은 접근 수준,
범위, 사이트 승인, 설정 잠금, DevTools 허용 목록을 **모두 우회**하므로 일반 사용자용 빌드에는
절대 들어가지 않습니다.

- 잠금 4개가 모두 맞아야 활성화됩니다: 컴파일 기능 플래그(릴리스 빌드는 컴파일 실패),
  `--sta-test-hooks` 명령줄 스위치 **그리고** `STA_E2E=1` 환경 변수, 실제 프로필이 아닌
  명시적인 `--sta-data-dir`(아니면 종료 코드 2), 그리고 활성화되지 않은 빌드에서는 `tools/list`에
  나타나지도 않고 `unknown_tool`로만 답합니다.
- 활성화된 브라우저는 실행마다 `TEST HOOKS ARMED` 경고를 로그에 남깁니다.
- 자세한 내용(도구 목록, 잠금, MCP로 옮길 수 없는 테스트 목록, 콘솔 창 금지 규칙)은
  `docs/TESTING.md`를 보세요.

## 5. 문제 해결

| 증상 | 원인 / 해결 |
|---|---|
| 설정이 맞는지 모르겠음 | 설정 → AI agents (MCP) → **Test connection**, 또는 `sta-mcp.exe --check` |
| `browser_not_running` | sta가 꺼져 있거나 Agent access가 꺼짐. sta를 열고 접근을 켜세요. `--no-launch`나 MSIX 클라이언트는 sta를 직접 실행하지 않습니다 |
| 다른 프로필을 봄 | `--data-dir`가 빠짐. 설정 화면의 스니펫을 그대로 복사하세요 |
| `not_approved` | sta 창 오른쪽 위(또는 설정 화면)에서 승인 후 다시 시도. 거부 직후 60초는 새 승인 창이 뜨지 않음 |
| 승인 창이 안 보임 | sta 창이 최소화되었거나 가려짐(작업 표시줄 버튼이 깜빡임). 타이핑 중에 뜬 창은 클릭해서 응답 |
| `paused` | Stop이 눌림. *Resume* 후 클라이언트에서 MCP 서버를 다시 시작 |
| `tab_not_visible` | `tab_show`로 탭을 표시하고, 최소화된 창을 복원 |
| `stale_ref` | 페이지가 바뀜. `page_snapshot`을 다시 호출 |
| `too_large` | 결과가 채널 한 줄(8 MiB)에 들어가지 않음. 그 호출만 실패하고 세션은 그대로 살아 있으니, `ref`·`offset`·`maxTokens`를 줄이거나(스크린샷은 `format: "jpeg"`·작은 `maxDimension`) 데이터를 파일로 저장하세요 |
| `scripts_disabled` / `history_disabled` / `downloads_disabled` | 설정의 Scripts, history and downloads에서 해당 항목을 켬 |
| 샌드박스 클라이언트가 연결 안 됨 | AppContainer·낮은 무결성 수준 프로세스는 보안 설계상 차단됨. 일반 권한으로 실행 |
| 파이프 접근 거부 | sta와 클라이언트가 다른 Windows 사용자 또는 다른 로그온 세션에서 실행됨 |
| Claude Code에서 결과가 잘리거나 파일로 저장됨 | Claude Code는 실제 토큰 수가 `MAX_MCP_OUTPUT_TOKENS`(기본 25000)를 넘는 결과를 오류로 바꾸고, 약 5만 자를 넘는 결과는 파일로 저장한 뒤 미리보기만 보여 줍니다. `maxTokens`, `root`, `offset`으로 적게 요청하세요(sta는 결과를 4만 자 이하로 자름) |
| `claude mcp add`가 `--data-dir`를 거부함 | npm 설치 + PowerShell. `claude.cmd mcp add …`로 실행 |

로그: `<데이터 폴더>\Logs\agent.log`, `<데이터 폴더>\Logs\sta.log`. MCP 서버의 진단 메시지는
stderr로 출력됩니다(Claude Code는 `claude --debug`).

자세한 도구 입력값, 기본값, 제한, 오류 코드, 위협 모델은 [`docs/MCP.md`](MCP.md)를 참고하세요.
