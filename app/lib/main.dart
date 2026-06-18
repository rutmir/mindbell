// MindBell — простой BLE-конфигуратор.
// Сканирует устройство "MindBell", читает/пишет расписание и синхронизирует время.
//
// UUID должны совпадать с прошивкой (firmware/src/main.rs):
//   сервис      6d696e64-6265-6c6c-0000-000000000000
//   расписание  ...0001  (JSON UTF-8, R/W)
//   время       ...0003  (u64 LE, локальный epoch, W)

import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_blue_plus/flutter_blue_plus.dart';
import 'package:permission_handler/permission_handler.dart';

const String kDeviceName = 'MindBell';
final Guid kServiceUuid = Guid('6d696e64-6265-6c6c-0000-000000000000');
final Guid kCharSchedule = Guid('6d696e64-6265-6c6c-0000-000000000001');
final Guid kCharTime = Guid('6d696e64-6265-6c6c-0000-000000000003');

void main() => runApp(const MindBellApp());

class MindBellApp extends StatelessWidget {
  const MindBellApp({super.key});
  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'MindBell',
      theme: ThemeData(colorSchemeSeed: Colors.indigo, useMaterial3: true),
      home: const ScanPage(),
    );
  }
}

// ─────────────────────────── модель ───────────────────────────

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
}

String fmtMin(int m) =>
    '${(m ~/ 60).toString().padLeft(2, '0')}:${(m % 60).toString().padLeft(2, '0')}';

// ─────────────────────────── скан ───────────────────────────

class ScanPage extends StatefulWidget {
  const ScanPage({super.key});
  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  List<ScanResult> _results = [];
  bool _scanning = false;

  @override
  void initState() {
    super.initState();
    FlutterBluePlus.scanResults.listen((r) {
      if (mounted) setState(() => _results = r);
    });
    FlutterBluePlus.isScanning.listen((s) {
      if (mounted) setState(() => _scanning = s);
    });
  }

  Future<bool> _ensureReady() async {
    // 1. Аппаратная поддержка BLE.
    if (!await FlutterBluePlus.isSupported) {
      _toast('Это устройство не поддерживает Bluetooth');
      return false;
    }
    // 2. Разрешения. На Android 12+ нужны Scan+Connect; на 11 и старше — гео.
    //    (Должны быть объявлены и в AndroidManifest.xml — см. README.)
    final st = await [
      Permission.bluetoothScan,
      Permission.bluetoothConnect,
      Permission.locationWhenInUse,
    ].request();
    final scanOk = st[Permission.bluetoothScan]?.isGranted ?? false;
    final connectOk = st[Permission.bluetoothConnect]?.isGranted ?? false;
    final locOk = st[Permission.locationWhenInUse]?.isGranted ?? false;
    final modernOk = scanOk && connectOk; // Android 12+
    if (!modernOk && !locOk) {
      _toast('Нет разрешений Bluetooth/геолокации — выдай их в настройках приложения');
      return false;
    }
    // 3. Адаптер включён?
    if (FlutterBluePlus.adapterStateNow != BluetoothAdapterState.on) {
      _toast('Включи Bluetooth');
      try {
        await FlutterBluePlus.turnOn(); // Android покажет системный запрос
      } catch (_) {}
      if (FlutterBluePlus.adapterStateNow != BluetoothAdapterState.on) return false;
    }
    return true;
  }

  Future<void> _startScan() async {
    if (!await _ensureReady()) return;
    setState(() => _results = []);
    try {
      await FlutterBluePlus.startScan(timeout: const Duration(seconds: 10));
    } catch (e) {
      _toast('Ошибка сканирования: $e');
    }
  }

  void _toast(String m) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));
  }

  String _name(ScanResult r) {
    final a = r.advertisementData.advName;
    return a.isNotEmpty ? a : r.device.platformName;
  }

  @override
  Widget build(BuildContext context) {
    // MindBell — наверх, остальные именованные — ниже.
    final named = _results.where((r) => _name(r).isNotEmpty).toList()
      ..sort((a, b) {
        final am = _name(a) == kDeviceName ? 0 : 1;
        final bm = _name(b) == kDeviceName ? 0 : 1;
        return am.compareTo(bm);
      });

    return Scaffold(
      appBar: AppBar(title: const Text('MindBell — поиск')),
      body: ListView(
        children: [
          for (final r in named)
            ListTile(
              leading: Icon(
                _name(r) == kDeviceName ? Icons.notifications_active : Icons.bluetooth,
                color: _name(r) == kDeviceName ? Colors.indigo : null,
              ),
              title: Text(_name(r)),
              subtitle: Text('${r.device.remoteId}   rssi ${r.rssi}'),
              onTap: () async {
                await FlutterBluePlus.stopScan();
                if (!mounted) return;
                Navigator.of(context).push(MaterialPageRoute(
                  builder: (_) => ConfigPage(device: r.device),
                ));
              },
            ),
          if (named.isEmpty && !_scanning)
            const Padding(
              padding: EdgeInsets.all(24),
              child: Text('Нажми «Сканировать». Не забудь нажать RST на плате — '
                  'она рекламируется только ~30 с после включения.'),
            ),
        ],
      ),
      floatingActionButton: FloatingActionButton.extended(
        onPressed: _scanning ? null : _startScan,
        icon: Icon(_scanning ? Icons.hourglass_top : Icons.search),
        label: Text(_scanning ? 'Сканирую…' : 'Сканировать'),
      ),
    );
  }
}

// ─────────────────────────── конфигуратор ───────────────────────────

class ConfigPage extends StatefulWidget {
  final BluetoothDevice device;
  const ConfigPage({super.key, required this.device});
  @override
  State<ConfigPage> createState() => _ConfigPageState();
}

class _ConfigPageState extends State<ConfigPage> {
  bool _busy = true;
  String _status = 'Подключение…';
  List<Segment> _segments = [];
  BluetoothCharacteristic? _schedChar;
  BluetoothCharacteristic? _timeChar;

  @override
  void initState() {
    super.initState();
    _connect();
  }

  Future<void> _connect() async {
    try {
      await widget.device.connect(timeout: const Duration(seconds: 15));
      try {
        await widget.device.requestMtu(247); // чтобы JSON влез одним пакетом
      } catch (_) {}
      final services = await widget.device.discoverServices();
      final svc = services.firstWhere(
        (s) => s.uuid == kServiceUuid,
        orElse: () => throw 'Сервис MindBell не найден',
      );
      for (final c in svc.characteristics) {
        if (c.uuid == kCharSchedule) _schedChar = c;
        if (c.uuid == kCharTime) _timeChar = c;
      }
      if (_schedChar == null) throw 'Характеристика расписания не найдена';
      await _readSchedule();
    } catch (e) {
      _set('Ошибка: $e', busy: false);
    }
  }

  Future<void> _readSchedule() async {
    _set('Читаю расписание…');
    try {
      final bytes = await _schedChar!.read();
      final text = utf8.decode(bytes);
      final obj = jsonDecode(text) as Map<String, dynamic>;
      final segs = (obj['segments'] as List)
          .map((e) => Segment.fromJson(e as Map<String, dynamic>))
          .toList();
      setState(() {
        _segments = segs;
        _busy = false;
        _status = 'Прочитано: ${segs.length} сегм.';
      });
    } catch (e) {
      _set('Не удалось прочитать расписание: $e', busy: false);
    }
  }

  Future<void> _writeSchedule() async {
    _set('Пишу расписание…');
    try {
      final json = jsonEncode({'segments': _segments.map((s) => s.toJson()).toList()});
      await _schedChar!.write(utf8.encode(json), withoutResponse: false, allowLongWrite: true);
      _set('Расписание сохранено ✓', busy: false);
    } catch (e) {
      _set('Ошибка записи: $e', busy: false);
    }
  }

  Future<void> _syncTime() async {
    if (_timeChar == null) {
      _set('Характеристика времени не найдена', busy: false);
      return;
    }
    _set('Синхронизирую время…');
    try {
      final now = DateTime.now();
      // Локальный epoch = UTC-секунды + смещение зоны (прошивка трактует время как локальное).
      final localEpoch =
          now.millisecondsSinceEpoch ~/ 1000 + now.timeZoneOffset.inSeconds;
      final data = ByteData(8)..setUint64(0, localEpoch, Endian.little);
      await _timeChar!.write(data.buffer.asUint8List(), withoutResponse: false);
      _set('Время синхронизировано (${now.hour.toString().padLeft(2, '0')}:'
          '${now.minute.toString().padLeft(2, '0')}) ✓', busy: false);
    } catch (e) {
      _set('Ошибка времени: $e', busy: false);
    }
  }

  void _set(String s, {bool busy = true}) {
    if (mounted) setState(() {
      _status = s;
      _busy = busy;
    });
  }

  Future<void> _pickTime(Segment seg, bool isStart) async {
    final cur = isStart ? seg.startMin : seg.endMin;
    final picked = await showTimePicker(
      context: context,
      initialTime: TimeOfDay(hour: cur ~/ 60, minute: cur % 60),
    );
    if (picked == null) return;
    setState(() {
      final m = picked.hour * 60 + picked.minute;
      if (isStart) {
        seg.startMin = m;
      } else {
        seg.endMin = m;
      }
    });
  }

  @override
  void dispose() {
    widget.device.disconnect();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('MindBell — настройка'),
        actions: [
          IconButton(
            tooltip: 'Перечитать с устройства',
            onPressed: _busy ? null : _readSchedule,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      body: Column(
        children: [
          Container(
            width: double.infinity,
            color: Theme.of(context).colorScheme.surfaceContainerHighest,
            padding: const EdgeInsets.all(12),
            child: Text(_status),
          ),
          if (_busy) const LinearProgressIndicator(),
          Expanded(
            child: ListView.builder(
              itemCount: _segments.length,
              itemBuilder: (_, i) => _segmentCard(_segments[i], i),
            ),
          ),
        ],
      ),
      floatingActionButton: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          FloatingActionButton.extended(
            heroTag: 'add',
            onPressed: () => setState(() => _segments.add(
                Segment(startMin: 540, endMin: 1200, intervalMin: 7, enabled: true))),
            icon: const Icon(Icons.add),
            label: const Text('Сегмент'),
          ),
          const SizedBox(height: 8),
          FloatingActionButton.extended(
            heroTag: 'time',
            backgroundColor: Colors.teal,
            onPressed: _busy ? null : _syncTime,
            icon: const Icon(Icons.access_time),
            label: const Text('Время'),
          ),
          const SizedBox(height: 8),
          FloatingActionButton.extended(
            heroTag: 'save',
            onPressed: _busy ? null : _writeSchedule,
            icon: const Icon(Icons.save),
            label: const Text('Сохранить'),
          ),
        ],
      ),
    );
  }

  Widget _segmentCard(Segment s, int i) {
    return Card(
      margin: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
      child: Padding(
        padding: const EdgeInsets.all(12),
        child: Column(
          children: [
            Row(
              children: [
                Expanded(
                  child: OutlinedButton.icon(
                    onPressed: () => _pickTime(s, true),
                    icon: const Icon(Icons.login, size: 18),
                    label: Text('с ${fmtMin(s.startMin)}'),
                  ),
                ),
                const SizedBox(width: 8),
                Expanded(
                  child: OutlinedButton.icon(
                    onPressed: () => _pickTime(s, false),
                    icon: const Icon(Icons.logout, size: 18),
                    label: Text('до ${fmtMin(s.endMin)}'),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                const Text('каждые '),
                SizedBox(
                  width: 64,
                  child: TextFormField(
                    initialValue: s.intervalMin.toString(),
                    keyboardType: TextInputType.number,
                    textAlign: TextAlign.center,
                    decoration: const InputDecoration(isDense: true),
                    onChanged: (v) => s.intervalMin = int.tryParse(v) ?? s.intervalMin,
                  ),
                ),
                const Text(' мин'),
                const Spacer(),
                Switch(
                  value: s.enabled,
                  onChanged: (v) => setState(() => s.enabled = v),
                ),
                IconButton(
                  icon: const Icon(Icons.delete_outline),
                  onPressed: () => setState(() => _segments.removeAt(i)),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
