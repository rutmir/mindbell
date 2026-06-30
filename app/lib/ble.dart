// BLE-слой MindBell: идентификаторы, модель сегмента и проверка готовности.
//
// UUID должны совпадать с прошивкой (firmware/src/main.rs):
//   сервис      6d696e64-6265-6c6c-0000-000000000000
//   расписание  ...0001  (JSON UTF-8, R/W)
//   время       ...0003  (W) 10 байт: UTC-epoch u64 LE + смещение зоны i16 LE (мин)

import 'package:flutter_blue_plus/flutter_blue_plus.dart';
import 'package:permission_handler/permission_handler.dart';

class MindBell {
  static const String deviceName = 'MindBell';
  static final Guid service = Guid('6d696e64-6265-6c6c-0000-000000000000');
  static final Guid charSchedule = Guid('6d696e64-6265-6c6c-0000-000000000001');
  static final Guid charTime = Guid('6d696e64-6265-6c6c-0000-000000000003');

  /// Готовит телефон к работе с BLE. Возвращает `null`, если всё хорошо,
  /// иначе — текст ошибки для показа пользователю.
  static Future<String?> ensureReady() async {
    if (!await FlutterBluePlus.isSupported) {
      return 'Это устройство не поддерживает Bluetooth';
    }
    // Разрешения СНАЧАЛА: без BLUETOOTH_CONNECT плагин не видит состояние
    // адаптера (adapterState вернёт unknown).
    final st = await [
      Permission.bluetoothScan,
      Permission.bluetoothConnect,
      Permission.locationWhenInUse,
    ].request();
    final scanOk = st[Permission.bluetoothScan]?.isGranted ?? false;
    final connectOk = st[Permission.bluetoothConnect]?.isGranted ?? false;
    final locOk = st[Permission.locationWhenInUse]?.isGranted ?? false;
    if (!(scanOk && connectOk) && !locOk) {
      return 'Нет разрешений Bluetooth — выдай их в настройках приложения';
    }
    final state = await _waitAdapterOn();
    if (state != BluetoothAdapterState.on) {
      return state == BluetoothAdapterState.unauthorized
          ? 'Нет разрешения Bluetooth — выдай его в настройках'
          : 'Включи Bluetooth и повтори';
    }
    return null;
  }

  /// Ждёт, пока адаптер перейдёт в `on` (поначалу adapterStateNow = unknown,
  /// пока поток adapterState не выдаст первое значение). При необходимости
  /// пробует включить адаптер.
  static Future<BluetoothAdapterState> _waitAdapterOn() async {
    BluetoothAdapterState state;
    try {
      state = await FlutterBluePlus.adapterState
          .firstWhere((s) => s != BluetoothAdapterState.unknown)
          .timeout(const Duration(seconds: 5));
    } catch (_) {
      state = FlutterBluePlus.adapterStateNow;
    }
    if (state == BluetoothAdapterState.on) return state;
    if (state == BluetoothAdapterState.off) {
      try {
        await FlutterBluePlus.turnOn();
      } catch (_) {}
    }
    try {
      state = await FlutterBluePlus.adapterState
          .firstWhere((s) => s == BluetoothAdapterState.on)
          .timeout(const Duration(seconds: 10));
    } catch (_) {
      state = FlutterBluePlus.adapterStateNow;
    }
    return state;
  }
}

/// Один диапазон расписания. Время — минуты от полуночи.
class Segment {
  int startMin;
  int endMin;
  int intervalMin;
  bool enabled;

  Segment({
    required this.startMin,
    required this.endMin,
    required this.intervalMin,
    required this.enabled,
  });

  factory Segment.fromJson(Map<String, dynamic> j) => Segment(
        startMin: (j['start_min'] ?? 0) as int,
        endMin: (j['end_min'] ?? 0) as int,
        intervalMin: (j['interval_min'] ?? 0) as int,
        enabled: (j['enabled'] ?? true) as bool,
      );

  Map<String, dynamic> toJson() => {
        'start_min': startMin,
        'end_min': endMin,
        'interval_min': intervalMin,
        'enabled': enabled,
      };

  /// Сколько сигналов даст сегмент за день (прошивка бьёт start, start+int, …
  /// строго меньше end).
  int get pulsesPerDay {
    if (intervalMin <= 0 || endMin <= startMin) return 0;
    return ((endMin - startMin - 1) ~/ intervalMin) + 1;
  }
}

String fmtMin(int m) =>
    '${(m ~/ 60).toString().padLeft(2, '0')}:${(m % 60).toString().padLeft(2, '0')}';
