# darkPyonix VSCode Integration Design

## 개요

darkPyonix manager(FastAPI 기반)가 VSCode Web 서버를 라우팅으로 연결하는 구조.
~~클라이언트는 Tauri 기반 WebView 앱으로 데스크탑, Android, iOS를 지원.~~
> **폐기됨 (2026-10-03 사용자 결정).** Tauri 클라이언트는 쓰지 않습니다. 클라이언트는 dioxus-compose 네이티브 앱이고,
> 이 문서의 VSCode 런타임 선택과 모바일 WebView 최적화 내용은 IDE 창에 그대로 적용됩니다. (`docs/INTENT.md` D9, D10)

---

## 런타임 구조

```
Client (Tauri WebView — 폐기됨, 2026-10-03)
    ↕
darkPyonix Manager (FastAPI)
    ↕ routing
VSCode Web Server (OSE 빌드 or MS 공식 빌드)
```

---

## VSCode 런타임 선택

GUI에서 두 가지 런타임 선택 제공.

| 항목 | OSE | VSC |
|------|-----|-----|
| 빌드 | darkPyonix 직접 컴파일 | 유저가 직접 설치한 MS 공식 빌드 |
| 마켓플레이스 | Open VSX | MS Marketplace |
| 기본값 | ✓ | - |
| 라이선스 | MIT | MS 라이선스 (유저 책임) |

### VSC 런타임 설정 흐름

1. VSC 선택 시 "Microsoft VS Code가 설치되어 있어야 합니다" 안내 표시
2. 설치 가이드 복사 버튼 제공
3. 명령어 입력란 제공: "설치한 ./code의 존재를 확인할 수 있는 명령어를 입력하세요"
4. 입력된 명령 실행 및 결과 표시 (code --version 확인 용도)
5. 확인되면 VSC 런타임으로 `code serve-web` 실행

---

## 모바일 WebView 최적화 (Android 우선)

### 클릭 반응 개선
```css
* {
    -webkit-tap-highlight-color: transparent;
    touch-action: manipulation; /* 300ms 딜레이 제거 */
    -webkit-touch-callout: none;
    user-select: none;
}
```

### Safe Area 통합
```html
<meta name="viewport" content="width=device-width, initial-scale=1.0, viewport-fit=cover">
```
```css
padding-top: env(safe-area-inset-top);
padding-bottom: env(safe-area-inset-bottom);
padding-left: env(safe-area-inset-left);
padding-right: env(safe-area-inset-right);
```

### Android 뒤로가기 처리
> 폐기됨(2026-10-03 사용자 결정): Tauri 기반 구현. 뒤로가기 처리 요구 자체는 유지.

Tauri Kotlin 플러그인으로 Android 시스템 뒤로가기 이벤트를 가로채서 앱 레벨에서 처리.
WebView 히스토리 뒤로가기 동작 차단.

### 모바일 UI 인젝션
VSCode 타이틀바/메뉴 영역을 모바일 친화적으로 재배치.
`IS_TOUCH` 감지 기반으로 모바일 레이아웃 적용.

---

## Tauri 구조

> **폐기됨 (2026-10-03 사용자 결정).**

```
Rust (Tauri core)
    ↕ Tauri Plugin Bridge
Kotlin (Android 플러그인)
    ↕
Android API (뒤로가기, Safe Area 등)
```

---

## iOS

Android 안정화 이후 검토.
WKWebView JIT 제한이 있으나 VSCode Web 클라이언트의 실제 JS 연산이 가벼운 편이라 실측 후 판단.

---

## 미결 사항

- Android Chrome에서 VSCode Web 실제 성능 측정 (가정 단계)
- VSCode 타이틀바 모바일 UI 재배치 상세 설계
- `touch-action` VSCode 기본 설정 여부 확인
