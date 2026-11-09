섹션 식별과 이름 변경 설계 및 작업 계획
=======================================

상태: 구현 완료. 검증 범위와 실행 기준은 아래에 기록한다.
대상은 [이슈 #11]과 Sacho 0.4이며,
설계 기준 소스는 `de87595`이다.

섹션의 안정 ID와 출력 제목을 분리한다. 새로 제목을 지정한 섹션은
Markdown에 안정 ID도 기록한다. 주석 없는 과거 문서는 제목과 ID의 호환
해석, 가족 단위 설정 매핑, 일회성 CLI 매핑으로 읽는다. 과거 릴리스의
본문을 소급 수정하지 않는다.

[이슈 #11]: https://github.com/dahlia/sacho/issues/11


사용자 동작과 호환성
--------------------

`id`는 내부 그룹, 정렬 및 `--section`의 키다. `title`은 H3에 출력하는
한 줄 평문이고, `directory`는 fragment 목적지다. 각 값은 독립적으로
변경할 수 있지만, ID는 같은 구성요소를 계속 가리키도록 유지하는 것이
기본이다. ID를 다른 구성요소에 재사용하지 않는다.

`title` 생략은 기존 `id` 제목과 주석 없는 출력을 유지한다. `title`을
명시하면 제목이 ID와 같더라도 ID 주석을 출력한다. 새 `init`은 주석이
있으면 그 ID, 없으면 발견한 제목을 ID로 삼고, 실제 H3 평문을 별도
`title`로 적어 앞으로의 기록을 시작한다.
새 reader는 현재 `title`의 유무와 관계없이 기존 ID 주석을 읽는다.

구한문은 기존 Rust 및 JavaScript ID를 유지하고 Python에 별도 ID를 준다.

~~~~ toml
[[section-patterns]]
source = "crates/{name}"
id = "{name}"
title = "{name} (Rust)"
directory = "rust/{name}"

[[section-patterns]]
source = "packages/{name}"
id = "@gukhanmun/{name}"
title = "@gukhanmun/{name} (JavaScript/TypeScript)"
directory = "javascript/{name}"

[[section-patterns]]
source = "python/{name}"
id = "python/{name}"
title = "{name} (Python)"
directory = "python/{name}"
~~~~

옛 H3 `gukhanmun`은 기존 ID `gukhanmun`으로 해석되어
`changes.d/rust/gukhanmun/carried-from-1.0.0.md`로 간다. 옛
`@gukhanmun/napi`도 기존 ID로 해석되어 `changes.d/javascript/napi/`로 간다.
이번 fixture에는 매핑 설정이 필요 없다. 패키지 100개도 세 가족 설정으로
다룬다. 디렉터리를 바꾸어도 남아 있는 미출시 fragment를 자동 이동하지는
않는다.

주석을 모르는 옛 Sacho의 새 문서 carry는 지원하지 않는다. 새 문서를
읽고 수정하는 자동화는 새 Sacho를 사용해야 한다. 일반 Markdown 표시는
계속 가능하다. `title` 생략 구성의 출력 바이트 호환은 새 필드의 도입과
별개로 회귀 테스트한다.


설정 타입과 제목 렌더링
-----------------------

`SectionConfig.title`은 `Option<String>`, `SectionPatternConfig.title`은
선택적인 렌더 전용 `SectionTitleTemplate`이다. 생략과 명시는 직렬화
왕복에서 구분한다. `CompiledSection.id`는 계속 안정 ID다.
`CompiledSection.title: Option<String>`을 추가해 명시한 렌더 제목만
저장하고, Some이면 ID 주석도 출력한다. 유효 출력 제목은 이 값 또는
기존 ID다. 섹션 없는 region은 두 값 모두 None이다. 공개 API에는
rustdoc과 0.4 마이그레이션 안내를 추가한다.

제목에는 빈 문자열, 앞뒤 공백, CR/LF와 제어문자를 허용하지 않는다.
대소문자나 Unicode 정규화로 동치를 새로 만들지 않는다. Markdown 강조나
HTML을 설정 문법으로 받지 않고 평문을 안전하게 escape하여 출력한다.
출력 H3의 평문을 다시 읽으면 원래 제목과 같아야 한다. Sacho의 수학·
footnote 확장을 켠 Comrak 옵션으로 renderer 직후와 Hongdown 포맷 후에
모두 이 불변식을 검사한다. `$x$ API`, `C #`, `<a>`, `&amp;`, backslash와
강조·링크 구문을 예제로 둔다. 필요한 구문 문자와 끝의 ATX 닫힘 `#`을
escape하되 `(`, `)`, `/`, `@`는 불필요하게 escape하지 않는다. title 생략
경로에는 새 escape를 적용하지 않는다.

제목 템플릿은 `{name}` 변수를 사용하며, literal 중괄호는 같은 문자를
두 번 적어 escape한다. `source`에서
선언한 변수만 사용할 수 있고, 변수 반복과 생략, `/`를 포함하는 일반
문구를 허용한다. ID 및 디렉터리용 가역 패턴의 제약을 제목에 강제하지
않는다. 현재 제목의 자동 역변환은 모든 캡처를 한 번씩 포함하며 기존
`SectionPattern` 문법으로 가역인 부분집합만 지원한다. 나머지는 ID
주석이나 명시 매핑으로 읽는다.

명시적 섹션끼리는 `title.unwrap_or(id)`인 유효 출력 제목으로 로드 때
중복을 검사한다. 생성 섹션과 legacy fragment 섹션을 포함한 전체 출력
제목 중복은 실제 compile 대상 집합에서 검사한다. 서로 다른 ID의 같은
출력 제목은 오류다. 검사는 `--section` 필터 이전의 전체 활성 fragment
섹션 집합에 적용하므로 단일 섹션 preview도 전체 제목 충돌을 보고한다.
무한한 패턴 공간 전체의 중복이 정적으로 검증된다고 약속하지
않는다. 변수 생략은 이름이 겹치는 실제 패키지에서 충돌할 수 있다.
scope를 생략한 `{name}`은 실제 출력 이름이 겹치지 않을 때만 유효하다.
소스 디렉터리를 열거해 해석 후보를 정하지 않는다.

title을 생략한 ID에 Markdown 구문이나 앞뒤 공백이 있으면, 기존 H3
출력의 평문과 ID가 다를 수 있다. 예를 들어 `*a*`와 `a`는 같은 평문으로
보인다. 이 기존 한계는 기본 출력 바이트 호환과 함께 남으며, 이러한
ID는 유효한 평문 title을 명시하여 ID 주석으로 보존하도록 안내한다.

legacy fragment 섹션은 디스크에서 발견됐지만 현재 resolver가 없음 또는
Shadowed로 답하는 섹션을 뜻한다. compile은 현재처럼 ID를 제목으로,
주석 없이 출력하고 기존 순서를 유지한다. title 조회 때문에 새로
실패시키지 않는다. 그 밖의 모호성 오류는 그대로 전파한다. 이 legacy
제목도 전체 유효 제목 충돌 검사에는 포함한다.

안정 ID는 기존 `Config::validate`와 같이 공백만 있는 값을 거부하되
그 밖의 문자열은 trim·정규화하지 않는 opaque key다. 기존 ID에 새로
제목의 제어문자·앞뒤 공백 금지를 적용하지 않는다. 예를 들어 ID가
`" key "` 또는 `"foo\nbar"`이고 별도 평문 title이 유효하면, 주석은 이를
encoding하여 정확히 보존한다. 주석 없는 과거 H3에서는 trim 때문에
복구할 수 없던 ID도 새 주석에서는 식별 가능하다.


ID 주석 형식과 문서 계약
------------------------

~~~~ markdown
### gukhanmun (Rust)

<!-- sacho:section:v1 id=gukhanmun -->

 -  Added support.
~~~~

주석은 해당 릴리스의 직접 H3 다음 첫 비공백 블록이어야 한다. 대상
영역의 모든 descendant HTML 블록·인라인 노드에서 예약 prefix를 검사한다.
직접 H3에 결합된 단독 유효 HTML 블록 외의 예약 주석은 항목 내부·
인용문·intro 안을 포함해 오류다. 한 H3의 여러 주석과 항목 뒤의 주석도
오류다. 코드 블록·인라인 코드·escape된 일반 텍스트만 예제로 허용한다.

HTML 노드의 `literal.trim()`은 위 예시의
`<!-- sacho:section:v1 id=<token> -->` 전체 형식과 정확히 일치해야 한다.
연속 주석·줄 끝 여분 텍스트·내부 공백 변형은 유효 marker가 아니다.
예약 prefix가 있는 손상 형식을 legacy 문서로 조용히 취급하지 않는다.

carry는 선택한 릴리스 body, import는 선택한 unreleased 영역, 새 release는
생성할 영역만 엄격 검증한다. 다른 릴리스나 서문의 손상은 무관한 버전의
작업을 차단하지 않는다. 이 격리는 대상 영역과 Markdown 블록 구조를
정상적으로 구분할 수 있는 문서의 ID 주석 검증에 적용한다. 앞선 미종결
HTML 주석 등으로 대상 구조 자체가 가려진 문서의 복구는 보장하지 않는다.
discovery는 유효 주석만 결합하고 손상 주석에서는
제목 후보로 돌아간다. 표시용 show는 유효하게 결합된 ID 주석만 제거하고
손상 주석은 원문대로 표시한다.

ID는 UTF-8 바이트를 percent encoding한다. ASCII 영숫자와 `._~`만
그대로 두고 나머지는 대문자 `%HH`로 기록한다. `/`, `%`, `-`, `<`, `>`도
encoding하여 주석 종료나 이중 하이픈을 만들지 않는다. 빈 ID, 잘못된
escape, 잘못된 UTF-8, 알 수 없는 버전은 오류다. reader는 소문자 hex도
허용하지만 writer는 항상 canonical 형식을 낸다. percent decoding은
정확히 한 번 수행한다. token에는 위 허용 문자와 `%HH`만 허용한다.
미인코딩 `/`가 있는 token도 오류다.
디코딩한 ID도 위의 기존 ID validator를 통과해야 한다. `%20`처럼 공백만
있는 ID는 오류지만, 구성에서 유효한 ID 내부의 공백·제어 바이트는
encoding된 상태로 보존한다. 손상 encoding과 유효한 opaque ID를 구분한다.

주석의 ID가 기준이며 현재 제목과 달라도 오류가 아니다. 알 수 없는
ID는 ID 매핑 또는 CLI로 해결하고 제목으로 조용히 대체하지 않는다.
같은 ID가 여러 H3에 기록되면 문서 순서대로 항목을 합친다.
주석 삭제는 legacy 문서와 구별할 수 없어 호환 해석으로 돌아간다.
손상된 대상 주석은 CLI로 우회하지 못한다. 오류에는 원래 위치와, 주석을
수동 수정하거나 삭제한 뒤 명시 매핑으로 복구하는 방법을 안내한다.

새 예약 prefix `sacho:section:`을 fragment와 release note/intro 입력에
허용하지 않는다. 코드 예제의 문자열은 허용한다. ID 주석은 fragment
본문이나 carry 항목으로 복사하지 않는다. `fmt`와 새 release/merge는
주석을 보존한다. 표시용 `show`는 ID 주석만 제거하고 일반 HTML 주석은
유지한다. 보관용 released span 추출 API는 원문을 그대로 돌려준다.


가족 단위 영구 매핑과 CLI
-------------------------

~~~~ toml
[[section-maps]]
kind = "title"
pattern = true
from = "{name} (Rust crate)"
to = "{name}"

[[section-maps]]
kind = "id"
pattern = true
from = "legacy-rust/{name}"
to = "{name}"
~~~~

`kind`는 필수 `title` 또는 `id`다. `pattern` 기본값은 false이며 이때
양쪽은 중괄호를 포함해 literal이다. true이면 `from`은 최소 한 캡처를
포함하는 기존 가역 패턴이다. 캡처 없는 규칙에는 literal을 사용한다.
`to`는 캡처를 치환한 현재 ID다. 선언되지 않은 변수는 오류지만 `to`의
변수 생략은 허용하여 가족 전체를 한 대상으로 합칠 수 있다. 경로
capture의 안전 규칙은 기존과 같다.

대상은 반드시 현재 ID resolver로 직접 해석한다. 매핑을 다시 따라가지
않는다. 여러 시대의 이름은 모두 현재 ID를 가리키는 평면 규칙으로 쓴다.
명시적 현재 섹션과 패턴의 기존 shadowing/모호성 규칙을 그대로 적용한다.
literal 대상은 설정 로드 때, pattern 대상은 구체 입력과 일치할 때
검증한다. literal target이 삭제되면 관련 없는 명령의 설정 로드도
실패하므로 해당 이력도 함께 정리한다. 새 `SectionMapConfig`만 unknown
field를 거부해 `patern` 같은 오타를 잡는다. 매핑은 carry에 적용하며
`--section` 별칭을 만들지 않는다.

`kind=title`의 FROM은 trim된 제목과 비교하므로 빈 값·앞뒤 공백·제어문자를
거부한다. `kind=id`의 literal FROM과 모든 literal TO는 기존 ID와 같은
opaque key 규칙을 쓰며 자동 trim하지 않는다. pattern의 고정 부분도
입력 영역의 규칙을 따르고, 렌더된 TO는 현재 ID로 검증한다. 따라서
유효한 기존 ID를 새 map 문법이 표현하지 못하는 일이 없도록 한다.

~~~~ sh
sacho carry 1.0.0 --map 'Old title' 'current-id'
sacho carry 1.0.0 --map-pattern 'legacy/{name}' 'python/{name}'
~~~~

두 옵션은 각각 FROM과 TO 두 인자를 받고 반복 가능하다. literal
`--map`은 공백이나 `=`, `->`, 중괄호가 있는 이름도 그대로 받는다.
`--map-pattern`만 템플릿을 해석한다. TO는 출력 제목이 아닌 현재 안정
ID다. 유효한 ID 주석이 있으면 FROM의 입력 키는 기록된 ID이고, 없으면
H3의 평문 제목이다. 대상 릴리스의 각 섹션에 적용하고 설정은 저장하지
않는다. ID 주석과 현재 ID를 의도적으로 override할 수 있다. CLI FROM은
주석 ID일 수 있으므로 opaque key로 비교하고, TO도 현재 ID로 검증한다.
공백만 있는 값은 거부하되 앞뒤 공백을 자동 제거하지 않는다. 주석 없는
입력에서 불필요한 공백을 넣으면 미사용 오류와 실제 입력 키로 안내한다.
leading hyphen은 값으로 허용한다. 오류에는 제목과 주석 ID를 함께 표시한다.

같은 영역과 같은 literal/pattern 종류의 중복 source 규칙은 같은
대상이더라도 오류다. 서로 다른 pattern이 같은 canonical 대상으로
가면 합치고, 다른 대상으로 가면 오류다. literal은 pattern의 예외다.
구체 입력에 후보가 여러 개 남으면 선언 순서로 선택하지 않는다.
CLI 문법과 모든 literal target을 먼저 검증한다. 다음으로 항목 수와
관계없이 대상 H3의 입력 키에 한 번이라도 일치하면 사용으로 센다.
literal에 가려진 pattern도 입력과 일치하면 사용이다. 다만 target은
선택된 우선순위에서만 구체 해석한다. 이후 미사용 규칙을 쓰기 전에
오류로 보고한다. 항목이 없는 H3도 사용 판정의 입력이다. 평문 제목 자체가
빈 H3는 섹션형 문서에서 형식 오류이며 CLI로 우회하지 못한다.
discovery는 빈 제목 H3를 후보에서 건너뛰는 기존 동작을 유지한다.


carry 해석 알고리즘
-------------------

선택한 영역을 구조적으로 파싱하여 각 H3에 ID 주석을 결합한다. 구조와
encoding이 손상된 주석은 CLI로도 우회하지 못한다. 제목 평문에는 현재와
같은 trim을 적용하되 새로운 case folding을 하지 않는다.

| 단계 | 주석 있는 섹션                        | 주석 없는 섹션                  |
| ---- | ------------------------------------- | ------------------------------- |
| 1    | CLI literal, 없으면 CLI pattern       | CLI literal, 없으면 CLI pattern |
| 2    | ID literal map, 없으면 ID pattern map | Title literal map               |
| 3    | 기록된 현재 ID                        | 명시한 현재 title               |
| 4    | 실패하면 unknown-ID 오류              | Title pattern map               |
| 5    | 제목 fallback 없음                    | ID literal map                  |
| 6    | 없음                                  | ID pattern map                  |
| 7    | 없음                                  | raw 현재 ID                     |

각 단계에서 후보를 canonical ID와 검증된 directory로 해석한다. 후보
없음만 다음 단계로 넘어간다. 일치한 규칙의 target 오류, 현재 title의
모호성, shadowing 오류는 다음 단계로 숨기지 않는다. 현재 제목 단계는
`title`을 명시한 섹션만 대상으로 하고, 생략한 섹션은 raw ID 단계에서
해석한다. 따라서 title 생략 패턴이 ID map을 가로막지 않는다. 해석 범위와
유효 출력 제목의 중복 검사 범위는 별개다. 현재 제목 해석은
명시적 제목의 정확 일치를 먼저 보고, 없으면 가역 제목 패턴의 모든
후보를 검사한다. 서로 다른 패턴의 다른 canonical 후보는 오류다.
생성 title candidate는 현재 ID resolver의 ID·directory와 일치해야 한다.
explicit override로 shadow된 패턴 제목을 다른 섹션에 붙이지 않고
shadowing 오류로 보고한다.

섹션 설정이 없으면 설정/CLI map은 `maps require sections` 오류다.
주석 없는 기존 문서는 현재처럼 root fragment로 평탄화한다. 새 예약
ID 주석이 있는 carry/import는 유효·손상을 불문하고 대상 영역의 예약
HTML 노드를 발견한 때 `section metadata requires sections`로
멈춰 식별을 조용히 버리지 않는다. 설정을 복원하거나 사람이 주석과
섹션 구조를 제거하여 평탄화를 명시적으로 선택해야 한다.

현재 title과 raw ID의 후보를 합집합으로 만들지 않는다. 예를 들어
`id = "{name}"`, `title = "{name} (Rust)"`에서 `gukhanmun (Rust)`는
제목으로 ID `gukhanmun`, raw ID로 ID `gukhanmun (Rust)`가 된다.
title 우선은 이 정상 문서를 읽지만, 주석 없는 과거 제목 재사용의 의미를
복구하지는 못한다. 그 경우 VERSION 하나에 대한 CLI override를 쓴다.
동일 입력 키의 항목을 서로 다른 섹션으로 분할하는 것은 범위 밖이다.

`{name}` 이력 규칙은 남은 한 세그먼트 제목을 그 가족으로 해석하겠다는
명시적 opt-in이다. 오타와 여전히 유효한 raw ID도 포획하여 재배치할 수
있음을 문서화한다. 현재 title보다 뒤에서 적용하여 새 제목을 다시
포획하는 일을 줄인다.


그룹화와 쓰기 불변식
--------------------

`released.rs`의 제목 검증만 바꾸지 않고 `ParsedEntry.section`에 처음부터
canonical ID를 저장한다. 현재 원제목 그룹 키와 `grouped.remove(current_id)`의
불일치가 항목을 조용히 누락시키지 않도록 잔여 그룹을 오류로 검사한다.

여러 옛 제목이 한 ID로 가면 원문 등장 순서대로 한
`carried-from-VERSION.md`에 합친다. 수치 priority는 frozen 문서에서
복구하지 않고 관측 순서를 보존한다. 텍스트를 임의로 중복 제거하지 않는다.
서로 다른 canonical ID의 목적지가 같으면 쓰기 전에 오류다.

기존 링크 destination 폴딩과 pin 일관성 검사를 유지한다. 같은 label의
다른 URL은 기존 오류로 실패하고 마지막 값으로 덮지 않는다. 소스
디렉터리 삭제는 ID 해석을 막지 않으며, 목적지의 경로/symlink 안전 검사는
계속 수행한다. 전체 계획 검증 후 기존 원자적 적용과 rollback을 사용한다.

`materialize=false`인 carry도 `after_sources`의 section 값 집합으로
유효 제목 충돌을 쓰기 전에 검사한다. 이 검사는 section 메타데이터만
사용하며 무관한 기존 fragment 본문을 새로 parse하지 않는다. 따라서
현재처럼 무관한 손상 fragment를 허용하는 비-materialize 경로는 유지한다.
materialize 경로와 같은 helper로 제목을 계산하고 pin 검사는 별도로 유지한다.

같은 설정·문서·주변 입력에서 carry 반복 결과는 동일한 경로와 바이트다.
materialize는 CHANGES.md를 쓸 수 있지만 released span은 바이트 단위로
불변이다. 수동 편집한 carry 파일의 재실행 덮어쓰기는 기존 동작을 유지한다.
매핑을 바꾼 뒤 예전 산출물 자동 정리와 기존 fragment 이동은 추가하지 않는다.


명령과 공개 API의 적용 범위
---------------------------

| 경로                             | 요구 동작                                                         |
| -------------------------------- | ----------------------------------------------------------------- |
| `add`, `preview --section`       | stable ID만 받음. 제목/역사 map을 별칭으로 받지 않음              |
| `compile`, `sync`, `release`     | title 렌더와 ID 주석 writer를 공통 사용                           |
| `preview`, compiled Markdown API | 실제 산출물을 보여주므로 ID 주석을 유지                           |
| `carry`                          | 위 역사 해석과 CLI 전체 지원                                      |
| `import-unreleased`              | ID 주석과 현재 title/ID 지원, 역사 map 및 carry CLI 미적용        |
| `fmt`, merge driver              | 소유 주석 보존, released 본문 보존, 동일 renderer 사용            |
| `show`                           | 인트로 유무와 무관하게 유효 ID 주석만 제거, 손상 주석은 원문 표시 |
| `init` / discovery               | ID 주석을 먼저 사용하고 새 구성에 title 명시                      |

기존 `carry_release(repo, changelog, version)`은 기본 옵션 wrapper로
유지한다. 새
`carry_release_with_options(repo, changelog, version, &CarryReleaseOptions)`를
추가하고 이 타입의 `runtime_maps` 기본값을 빈 배열로 둔다.
`RuntimeSectionMap`은 literal과 pattern을 구분하는 공개 타입이며 설정 map과
같은 내부 evaluator를 사용한다. command의 `CarryOptions`에도 runtime maps를
연결한다. 공개 구조체 literal과 `CompiledSection` 필드 추가의 source
compatibility 영향은 0.4 마이그레이션 문서에 적는다. CLI와 library가 같은 해석
함수를 사용하며 CLI만 별도의 문자열 치환을 하지 않는다.

init 후보에는 안정 `id`와 관측 `title`을 별도로 둔다. 동일 ID의 여러
제목은 기존의 unreleased 우선·문서 내 첫 등장 순서로 대표를 정한다.
주석 없는 후보의 초기 ID는 제목이다. 패턴 추론도 title을 별도로
추론해 편집 가능하게 제시한다. source capture로 일관되게 추론할 수
없으면 제목을 ID 템플릿으로 덮지 않고 explicit 섹션을 유지한다. 수기
TOML writer에 title 출력을 추가한다. `Config`, `SectionConfig`,
`SectionPatternConfig`, `CarryOptions`, `CompiledSection`,
`ChangelogSectionCandidate`의 새 필드 및 관련 discovery API 변경을
0.4 공개 API 마이그레이션 목록에 적는다. `infer_section_pattern`도
기존 ID 목록 대신 ID/title 쌍을 받아야 하므로 변경 목록에 명시한다.
관측 title에 제어문자 등 새 제목 규칙으로 표현할 수 없는 값이 있으면,
init은 설정을 쓰기 전에 유효한 제목으로 편집하도록 요구한다. 입력을
받을 수 없으면 오류로 종료하고 유효하지 않은 구성을 생성하지 않는다.


단계별 작업 계획
----------------

각 단계는 선행 단계의 계약 테스트가 통과한 뒤 진행한다. 중간 단계는
구현 단위이고, writer를 reader보다 먼저 활성화한 부분 릴리스를 내지 않는다.

| 단계 | 작업과 소유 파일                                                                                                            | 완료 기준                                                                    |
| ---- | --------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| A    | 구버전 fixture와 파이프라인 회귀: `crates/sacho/tests/section_identity.rs`, `crates/sacho/tests/fixtures/section-identity/` | 이슈의 archived 바이트·순서·링크와 현재 unknown-section 실패를 고정          |
| B    | `config.rs`, `section_pattern.rs`와 새 title 타입, `section.rs`                                                             | 설정 왕복, title/ID 해석 분리, 기본값의 출력 호환을 테스트                   |
| C    | 새 metadata 모듈, `released.rs`, `release_context.rs`, `fragment.rs`, `error.rs`                                            | 대상 범위 codec/결합/오류, 빈 maps로 전체 선택표 구현, import 정규화 완료    |
| D    | `compile.rs`, `commands.rs`, `cli.rs`, `merge.rs`, discovery/init, `show`                                                   | sync/release 주석 보존, fmt 전후 의미 동일, merge 후 ID 동일, show 표시 제거 |
| E    | map 설정/해석, `cli.rs`, command/library options                                                                            | 선택표·N:1·CLI/target 오류, 비-materialize 제목 충돌과 no-write 통과         |
| F    | 사용자 문서, rustdoc, changelog, 최종 검증                                                                                  | 아래 acceptance matrix와 프로젝트 finishing gates 통과                       |

단계 C와 D는 같은 feature release에 포함한다. B/C에서 reader 없이
title-only 문서를 만들지 않도록 writer 활성화는 D까지 보류한다.
각 구현 작업은 실패하는 동작 테스트를 먼저 작성하고, 수정 후 해당
회귀와 전체 정상 테스트를 진행한다.

F에는 옛 Sacho가 새 `title`/maps 필드를 조용히 무시해 제목을 ID로
되돌리는 혼용 위험도 적는다. 새 설정을 공유하는 자동화는 도구 버전도
함께 올린다. fmt/merge의 기존 formatting·충돌 처리를 바꾸지는 않으며,
새 주석의 보존과 의미 무결성을 추가 검증한다.


검증과 승인 기준
----------------

| 영역      | 고정 예제와 속성                                                                                                           |
| --------- | -------------------------------------------------------------------------------------------------------------------------- |
| 과거 호환 | 이슈의 두 파일/네 항목/링크/순서, source 삭제, 옛 branch, history 없는 새 설정, 원래 새 ID 패턴과 title-map 이주           |
| 안정 ID   | title 두 번 변경/제거/title=id, title/raw ID 반례, title 생략 패턴의 ID map 도달, unknown marker ID와 매핑                 |
| 주석      | encoding 왕복, `%`/`-`/Unicode/opaque ID, 공백만 있는 ID, invalid UTF-8/escape/version, 고아/중복/항목 뒤/코드·intro       |
| 제목      | Sacho Comrak 및 Hongdown 후 평문 왕복, `$x$ API`/`C #`/`<a>`/`&amp;`, braces, 변수 반복·생략, explicit/생략/생성 제목 충돌 |
| 매핑      | title/id 영역, literal 예외, pattern overlap의 같은/다른 대상, dangling target, 단일 hop, N:1                              |
| CLI       | 공백/`=`/`->`/braces/leading hyphen, 빈 값, 가려진 pattern, 항목 없는 H3와 빈 제목 H3 구분, 주석 override, 미사용 오류     |
| 쓰기      | stable 항목 분할, pin/path/title 충돌 시 무변경, materialize true/false, 무관한 손상 fragment, released bytes, 반복 동일   |
| 통합      | import, init의 같은 ID·여러 제목, fmt/merge/show, 필터 전 제목 충돌, preview 주석, legacy None/Shadowed 출력, API          |
| 범위      | 다른 릴리스 손상에 영향 없음, nested/inline/intro 예약 주석 거부, no-section legacy 평탄화 및 새 metadata 거부             |

추가로 init의 invalid title 무변경 실패와, no-section의 유효·손상 예약
HTML 거부를 각각 고정 예제로 둔다.
비대상 영역의 닫힌 손상 marker와 미종결 HTML에 의한 문서 구조 손상을
구별하며, 후자의 성공적인 복구를 acceptance 조건으로 요구하지 않는다.

F의 finishing gates는 `mise run check`, `mise run test`다. 이번 작업에서는
사용자 지시에 따라 전체 workspace의 `mise run mutants`를 면제한다.
기존 기능 범위 집중 mutation 기록도 검증 증거로 함께 검토한다. 외부 준비
사본에서 제목/resolver 44개, metadata/reader 및 fmt 80개, 렌더 제목 검증
14개, 매핑 및 우선순위 해석 55개 mutant를 검사했고 각 범위에서 missed와
timed-out이 모두 0이었다.
이 기록은 당시 선택한 기능 범위의 증거이며, 최종 통합 소스 전체의 mutation
통과를 뜻하지 않는다. 최종 통합 소스는 check/test로 검증한다. 실제 VCS
adapter를 바꾸면
`mise run test:vcs`도 추가한다.


설계의 경계
-----------

과거 제목 재사용과 삭제된 ID 주석의 의미를 정보 없이 자동 복구하지 않는다.
일회성 CLI로 구분할 수 없는 동일 릴리스 내 1:N 분할은 수동 편집이 필요하다.
버전 범위 설정, VCS rename 추적, fragment 자동 이동은 이번에 추가하지 않는다.
숨은 ID 주석은 사람용 문서와 기계용 식별을 함께 보관하는 비용이며, 기존
문서 해석 경로와 함께 유지해야 한다.


설계 기준 소스의 근거
---------------------

아래 고정 링크는 설계 기준인 `de87595`의 구현을 가리킨다. 설계 당시의
동작과 변경 지점을 확인하기 위한 근거이며, 최종 통합 소스는 별도로 확인한다.

 -  [`SectionConfig` 및 Config]
 -  [가역 패턴과 캡처 안전 규칙]
 -  [현재 ID resolver]
 -  [H3 출력과 compiled API]
 -  [released 파싱과 carry 그룹화]
 -  [carry 계획, pin 검사와 원자적 쓰기]
 -  [intro 주석과 예약 경계 검증]

[`SectionConfig` 및 Config]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/config.rs
[가역 패턴과 캡처 안전 규칙]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/section_pattern.rs
[현재 ID resolver]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/section.rs
[H3 출력과 compiled API]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/compile.rs
[released 파싱과 carry 그룹화]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/released.rs
[carry 계획, pin 검사와 원자적 쓰기]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/commands.rs
[intro 주석과 예약 경계 검증]: https://github.com/dahlia/sacho/blob/de87595333ab66f519c1e815c847d46e1cab5fc6/crates/sacho/src/release_context.rs
