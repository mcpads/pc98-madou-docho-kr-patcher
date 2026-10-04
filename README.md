# 마도물어 도초이문 (PC-98) 한글 패처

Disc Station 3호에 수록된 PC-98용 《마도물어 ~도초이문~》을 독립 실행 디스크로 만들고 한글 패치를 적용하는 Rust 코드입니다. 원본 HDM·FDI 식별, 설치기(LHa 자기 해제 파일) 해제, Disc Station 4호의 공식 프리징 수정 재현, Compile-LZ 압축·해제, `MAIN.OVL`·게임 진행 DAT·외부 DOS 프로그램 문면 추출과 재삽입, 한글 외부 글꼴(`KFONT.BIN`)과 V30 렌더러 훅, 조사 선택, 오프닝·제목·엔딩 그래픽 교체, FAT12 디스크 조립과 BPS 생성을 제공합니다.

배포용 패치와 적용 방법은 [마도물어 시리즈 한글 번역 프로젝트](https://github.com/mcpads/madou-monogatari-kr-patch/tree/main/pc98-madou-docho)에서 제공합니다.

## 제공하지 않는 것

이 저장소에는 원본 디스크, 패치를 적용한 디스크, 번역 카탈로그 JSON, 글꼴 파일과 한글 제목 그래픽이 없습니다. 따라서 이 저장소만으로는 패치를 만들 수 없습니다. 아래 입력을 직접 갖춘 경우에만 한글 디스크와 BPS, 패치 ZIP이 생성됩니다.

## 빌드와 테스트

```bash
cargo build --release
cargo test
```

기본 테스트는 합성 입력만 사용합니다. 글꼴이 필요한 테스트는 `#[ignore = "requires ..."]`로 필요한 파일을 밝혀 두었습니다. 입력을 갖춘 뒤 `cargo test -- --ignored`로 실행하며, 입력이 없으면 성공으로 넘어가지 않고 실패합니다.

## 지원 원본

| 원본 | 크기 | SHA-256 |
| --- | --- | --- |
| Disc Station Vol. 03 Disk 1 (HDM) | 1,261,568바이트 | `2ba5ada68e76a74a2659484174b79ebf9a2c8285fab650e11a5340698d78b22e` |
| Disc Station Vol. 03 [Set 1] Disk 1 (HDM) | 1,261,568바이트 | `bcc7ca6f35fe057e8d1f8341353e77a93003062cff1872e10681eab18df99226` |
| Disc Station Vol. 03 Disk 1 다른 덤프 (HDM) | 1,261,568바이트 | `2ec8333fe9dc98e27d19a5ce9642651bbcb0e6654d8b0fa3cd769d15a81bc140` |
| 독립 실행판 `【PC98】魔導物語 道草異聞.fdi` (FDI BPS 전용) | 1,265,664바이트 | `cb6647a726861d7ba2d1cdd414cf21f6f35f892fb1f4065a8116f4367656d6b4` |

빌드는 입력 이미지 전체와 설치기 `MADOU.EXE`, 공식 수정 전후 `BSAMP.COM`의 SHA-256이 하나라도 다르면 산출물을 만들지 않습니다. 배포 페이지의 패치 ZIP은 첫 번째 HDM에만 적용됩니다. 4호 디스크는 입력으로 요구하지 않습니다.

## 빌드 입력

입력 디렉터리 기본값은 현재 디렉터리의 `assets`이며 `--assets <DIR>`로 바꿀 수 있습니다.

| 입력 | 경로 (`assets/` 기준) | 비고 |
| --- | --- | --- |
| 한글 글꼴 | `fonts/Galmuri14.ttf` | [Galmuri](https://github.com/quiple/galmuri) 2.404, 상류 리비전 `71e1cacf1437a11220307120e63e30bc275312d4` |
| 글꼴 래스터 설정 | `fonts/galmuri14-pc98-16x16.json` | 동봉, 실행 파일에 포함 |
| 제목 그래픽 | `graphics/title-art.json`, `graphics/title-composition.png` | 640×272 RGBA 합성 자산과 소유 영역·계보 manifest |
| `MAIN.OVL` 문면 | `translations/main/catalog.json`, `translations/main/entries/*.json` | |
| 상점·적 DAT 문면 | `translations/gameplay/catalog.json`, `translations/gameplay/resources/*.json` | |
| 오프닝 그래픽 문면 | `translations/opening-graphic.json` | |
| 제목·엔딩 그래픽 문면 | `translations/graphic-text.json` | |
| 외부 DOS 프로그램 문면 | `translations/external/catalog.json`, `translations/external/programs/*.json` | |

필요한 카탈로그 파일 목록은 `src/standalone.rs`의 `TRANSLATION_CATALOG_FILES`에 있습니다. 배포용 빌드는 모든 번역 항목이 `distribution_eligible` 상태이고 한국어 필드에 일본어 문자가 남지 않았을 때만 진행합니다.

글꼴은 재배포 조건을 이 저장소에서 보장할 수 없어 포함하지 않습니다. 라이선스는 배포처에서 확인하세요. 빌드는 글꼴의 SHA-256이 래스터 설정의 값과 다르면 진행하지 않습니다. 배포 패치 1.0.0은 다음 파일로 만들었습니다.

```text
6fe6c3fe4369e3837ac348431e8670733d67aa4bd550982baa72cc93c81a1c68  Galmuri14.ttf
47cb173fabdc0b0c57c4a1331b36a6f56e70c8e4599bdea8541aedf0abba10ce  title-art.json
a8b8a14f27267da79db20e9986580e3e10ad3806ea4d59b0a1715bc6ddbdc5a2  title-composition.png
```

## 패치 생성

```bash
B=target/release/pc98-madou-docho

# 원본 식별
$B verify-source "roms/Disc Station Vol. 03 (Disk 1).hdm"

# 한글 독립 실행 HDM
$B build-translation-test-image "roms/Disc Station Vol. 03 (Disk 1).hdm" out/docho-kr.hdm

# 독립 실행판 FDI용 BPS
$B build-fdi-release-patch "roms/Disc Station Vol. 03 (Disk 1).hdm" \
  "roms/【PC98】魔導物語 道草異聞.fdi" out/docho-kr.bps --target-output out/docho-kr.fdi
```

배포 패치 1.0.0과 같은 입력이면 한글 HDM의 SHA-256은 `4e54d3fd997247653a774c9867908889a54a822aadd4fff767bfd5e89c465776`입니다. FDI용 BPS는 `efcfa1e30f134c42976b22ab507c2a0da709e896053c07609f9913d564de4f11`, 적용 결과 FDI는 `b3a03ca53ccc8bae215e045f3af63f0504e63356e7dcf4c4ee51a5d3e19652ea`입니다. BPS 명령은 FDI 헤더를 보존하고, 만든 BPS를 원본에 다시 적용해 결과가 같은지 확인한 뒤에만 파일을 씁니다.

## 패치 ZIP 생성

배포 페이지의 파일별 BPS ZIP은 [`scripts/build-disc-station-vol03-patch-package.sh`](scripts/build-disc-station-vol03-patch-package.sh)가 만듭니다. 스크립트는 위 한글 HDM을 임시로 만들어 SHA-256을 확인한 뒤 [Retro Patcher](https://github.com/mcpads/retro-patcher)의 `patch-core`에 있는 `pc98_patch_author`로 [`release/disc-station-vol03-disk1.plan.json`](release/disc-station-vol03-disk1.plan.json)에 따라 ZIP을 두 번 생성해 바이트가 같은지 확인하고, 원본에 다시 적용해 봅니다. 원본과 결과 HDM은 ZIP에 들어가지 않습니다. 입력 디렉터리는 저장소 루트의 `assets`를 씁니다.

```bash
scripts/build-disc-station-vol03-patch-package.sh \
  "roms/Disc Station Vol. 03 (Disk 1).hdm" \
  path/to/retro-patcher \
  out/docho-kr.zip
```

배포 패치 1.0.0과 같은 입력이면 ZIP의 SHA-256은 `7789bea08a802693c98c2effaea30ab57ce89e2d9d6d5e2a281a2d8bdf90ecd6`입니다.

## 그 밖의 명령

문면 카탈로그 추출·검증(`extract-*-text`, `validate-*-text`), 단계별 개발 빌드(`build-*-development`), 그래픽 원본 렌더(`render-graphic-text-sources`), 설치기 조사(`survey`)의 사용법은 `cargo run -- help <명령>`으로 확인할 수 있습니다.

## 라이선스

이 저장소의 소스 코드는 [MIT License](LICENSE)로 제공합니다.
