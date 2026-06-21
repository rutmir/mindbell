# MindBell — телефонный конфигуратор (Flutter)

Простое приложение: сканирует BLE-устройство **MindBell**, читает/редактирует
расписание (с выбором времени по часам) и синхронизирует время. Пишет в те же
характеристики, что nRF Connect, только по-человечески.

UUID совпадают с прошивкой (`../firmware/src/main.rs`):
- сервис `6d696e64-6265-6c6c-0000-000000000000`
- расписание `…0001` (JSON), время `…0003` (u64 LE).

## Сборка (Android)

В этом каталоге лежат только `pubspec.yaml` и `lib/main.dart`. Платформенные
папки (`android/`) надо сгенерировать один раз:

```bash
cd app

# 1) Сгенерировать каркас Android (перезапишет наши файлы шаблоном — вернём их следом)
flutter create . --platforms=android --project-name mindbell

# 2) Вернуть наши pubspec.yaml и lib/main.dart (если flutter create их затёр)
git checkout -- pubspec.yaml lib/main.dart

# 3) Зависимости
flutter pub get
```

### Права BLE — добавить в `android/app/src/main/AndroidManifest.xml`

Сразу после строки `<manifest ...>` (до `<application>`):

```xml
<uses-permission android:name="android.permission.BLUETOOTH_SCAN"
    android:usesPermissionFlags="neverForLocation" />
<uses-permission android:name="android.permission.BLUETOOTH_CONNECT" />
<!-- для Android 11 и старше -->
<uses-permission android:name="android.permission.BLUETOOTH" android:maxSdkVersion="30" />
<uses-permission android:name="android.permission.BLUETOOTH_ADMIN" android:maxSdkVersion="30" />
<uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" android:maxSdkVersion="30" />
```

Если Gradle ругнётся на `minSdkVersion` — выстави в
`android/app/build.gradle` `minSdk = 21` (flutter_blue_plus требует ≥21).

## Запуск / установка

- **С подключённым по USB телефоном** (нужен включённый USB-debugging):
  ```bash
  flutter run --release
  ```
  ⚠️ В Qubes для этого телефон тоже надо пробросить в qube (`qvm-usb attach`).
- **Без проброса** — собрать APK и перекинуть на телефон вручную:
  ```bash
  flutter build apk --release
  # файл: build/app/outputs/flutter-apk/app-release.apk
  ```
  Скопировать APK на телефон (файлом/облаком) и поставить (разрешить установку
  из неизвестных источников).

## Как пользоваться

1. Нажми **RST** на плате (она рекламит BLE только ~30 с после старта).
2. В приложении → **Сканировать** → тапни **MindBell**.
3. **Время** (телефонная кнопка) — записать текущее локальное время в устройство.
4. Отредактируй сегменты (с ЧЧ:ММ / каждые N мин / вкл-выкл, можно добавлять и
   удалять) → **Сохранить**.

`start_min`/`end_min` в прошивке — минуты от полуночи; здесь они скрыты за
выбором времени. Ночная тишина = промежуток, не покрытый ни одним сегментом.
