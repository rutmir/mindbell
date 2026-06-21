// Экран настройки подключённого устройства: намерение, расписание, время.

import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/material.dart';
import 'package:flutter_blue_plus/flutter_blue_plus.dart';

import 'ble.dart';
import 'main.dart';

class ConfigPage extends StatefulWidget {
  final BluetoothDevice device;
  final String intention;
  const ConfigPage({super.key, required this.device, this.intention = ''});
  @override
  State<ConfigPage> createState() => _ConfigPageState();
}

enum _Conn { connecting, ready, error }

class _ConfigPageState extends State<ConfigPage> {
  _Conn _conn = _Conn.connecting;
  String _status = 'Подключение…';
  bool _busy = false;
  List<Segment> _segments = [];
  BluetoothCharacteristic? _schedChar;
  BluetoothCharacteristic? _timeChar;

  static const _intervalPresets = [3, 5, 7, 10, 15, 20, 30, 60];

  @override
  void initState() {
    super.initState();
    _connect();
  }

  Future<void> _connect() async {
    try {
      // License.nonprofit — бесплатный тариф flutter_blue_plus 2.x для личного
      // и некоммерческого использования.
      await widget.device.connect(
        license: License.nonprofit,
        timeout: const Duration(seconds: 15),
      );
      try {
        await widget.device.requestMtu(247); // чтобы JSON влез одним пакетом
      } catch (_) {}
      final services = await widget.device.discoverServices();
      final svc = services.firstWhere(
        (s) => s.uuid == MindBell.service,
        orElse: () => throw 'Сервис MindBell не найден',
      );
      for (final c in svc.characteristics) {
        if (c.uuid == MindBell.charSchedule) _schedChar = c;
        if (c.uuid == MindBell.charTime) _timeChar = c;
      }
      if (_schedChar == null) throw 'Характеристика расписания не найдена';
      await _readSchedule();
      if (mounted) setState(() => _conn = _Conn.ready);
    } catch (e) {
      if (mounted) {
        setState(() {
          _conn = _Conn.error;
          _status = 'Не удалось подключиться: $e';
        });
      }
    }
  }

  Future<void> _readSchedule() async {
    final bytes = await _schedChar!.read();
    final obj = jsonDecode(utf8.decode(bytes)) as Map<String, dynamic>;
    final segs = (obj['segments'] as List)
        .map((e) => Segment.fromJson(e as Map<String, dynamic>))
        .toList();
    if (mounted) setState(() => _segments = segs);
  }

  Future<void> _refresh() async {
    setState(() => _busy = true);
    try {
      await _readSchedule();
      _toast('Загружено с устройства');
    } catch (e) {
      _toast('Ошибка чтения: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _writeSchedule() async {
    setState(() => _busy = true);
    try {
      final json =
          jsonEncode({'segments': _segments.map((s) => s.toJson()).toList()});
      await _schedChar!
          .write(utf8.encode(json), withoutResponse: false, allowLongWrite: true);
      _toast('Расписание сохранено ✓');
    } catch (e) {
      _toast('Ошибка записи: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _syncTime() async {
    if (_timeChar == null) {
      _toast('Характеристика времени не найдена');
      return;
    }
    setState(() => _busy = true);
    try {
      final now = DateTime.now();
      final localEpoch =
          now.millisecondsSinceEpoch ~/ 1000 + now.timeZoneOffset.inSeconds;
      final data = ByteData(8)..setUint64(0, localEpoch, Endian.little);
      await _timeChar!.write(data.buffer.asUint8List(), withoutResponse: false);
      _toast('Время выставлено: ${fmtMin(now.hour * 60 + now.minute)} ✓');
    } catch (e) {
      _toast('Ошибка времени: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  void _toast(String m) {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(m)));
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
    return GradientScaffold(
      appBar: AppBar(
        backgroundColor: Colors.transparent,
        title: const Text('Настройка'),
        actions: [
          IconButton(
            tooltip: 'Перечитать с устройства',
            onPressed: _conn == _Conn.ready && !_busy ? _refresh : null,
            icon: const Icon(Icons.refresh),
          ),
        ],
      ),
      bottomBar: _conn == _Conn.ready ? _actionBar() : null,
      body: SafeArea(
        top: false,
        child: _conn == _Conn.ready ? _readyBody() : _statusBody(),
      ),
    );
  }

  Widget _statusBody() {
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            if (_conn == _Conn.connecting)
              const CircularProgressIndicator(color: Palette.teal)
            else
              const Icon(Icons.bluetooth_disabled,
                  size: 48, color: Palette.textDim),
            const SizedBox(height: 20),
            Text(_status, textAlign: TextAlign.center),
            if (_conn == _Conn.error) ...[
              const SizedBox(height: 20),
              FilledButton(
                onPressed: () {
                  setState(() {
                    _conn = _Conn.connecting;
                    _status = 'Подключение…';
                  });
                  _connect();
                },
                child: const Text('Повторить'),
              ),
            ],
          ],
        ),
      ),
    );
  }

  Widget _readyBody() {
    final total =
        _segments.where((s) => s.enabled).fold<int>(0, (a, s) => a + s.pulsesPerDay);
    return ListView(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 16),
      children: [
        if (widget.intention.isNotEmpty) ...[
          GlassCard(
            child: Row(
              children: [
                const Icon(Icons.center_focus_strong, color: Palette.teal),
                const SizedBox(width: 14),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      const Text('В фокусе',
                          style: TextStyle(
                              color: Palette.textDim, fontSize: 12)),
                      Text(widget.intention,
                          style: const TextStyle(
                              fontSize: 16, fontWeight: FontWeight.w600)),
                    ],
                  ),
                ),
              ],
            ),
          ),
          const SizedBox(height: 12),
        ],
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 4),
          child: Text(
            total > 0
                ? 'Около $total возвращений внимания в день'
                : 'Нет активных напоминаний',
            style: const TextStyle(color: Palette.textDim),
          ),
        ),
        const SizedBox(height: 4),
        ..._segments.asMap().entries.map((e) => _segmentCard(e.value, e.key)),
        const SizedBox(height: 8),
        OutlinedButton.icon(
          onPressed: () => setState(() => _segments.add(Segment(
              startMin: 540, endMin: 1200, intervalMin: 7, enabled: true))),
          icon: const Icon(Icons.add),
          label: const Text('Добавить интервал'),
        ),
        const SizedBox(height: 12),
        const Text(
          'Промежуток, не покрытый ни одним интервалом, — тишина (например, ночь). '
          'Часы на устройстве сбрасываются при потере питания — после этого '
          'нажми «Время».',
          style: TextStyle(color: Palette.textDim, fontSize: 12),
        ),
      ],
    );
  }

  Widget _segmentCard(Segment s, int i) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: GlassCard(
        child: Column(
          children: [
            Row(
              children: [
                Expanded(
                  child: _timeButton('с', s.startMin, () => _pickTime(s, true)),
                ),
                const SizedBox(width: 10),
                Expanded(
                  child: _timeButton('до', s.endMin, () => _pickTime(s, false)),
                ),
                Switch(
                  value: s.enabled,
                  onChanged: (v) => setState(() => s.enabled = v),
                ),
              ],
            ),
            const SizedBox(height: 4),
            Align(
              alignment: Alignment.centerLeft,
              child: Text('каждые ${s.intervalMin} мин  ·  ≈ ${s.pulsesPerDay}/день',
                  style: const TextStyle(color: Palette.textDim, fontSize: 13)),
            ),
            const SizedBox(height: 8),
            Wrap(
              spacing: 8,
              children: [
                for (final p in _intervalPresets)
                  ChoiceChip(
                    label: Text('$p'),
                    selected: s.intervalMin == p,
                    onSelected: (_) => setState(() => s.intervalMin = p),
                  ),
                IconButton(
                  visualDensity: VisualDensity.compact,
                  icon: const Icon(Icons.delete_outline),
                  color: Palette.textDim,
                  onPressed: () => setState(() => _segments.removeAt(i)),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  Widget _timeButton(String label, int min, VoidCallback onTap) {
    return OutlinedButton(
      onPressed: onTap,
      style: OutlinedButton.styleFrom(
        padding: const EdgeInsets.symmetric(vertical: 12),
        side: const BorderSide(color: Palette.cardBorder),
      ),
      child: Column(
        children: [
          Text(label,
              style: const TextStyle(color: Palette.textDim, fontSize: 11)),
          Text(fmtMin(min),
              style: const TextStyle(
                  fontSize: 18, fontWeight: FontWeight.w600, color: Colors.white)),
        ],
      ),
    );
  }

  Widget _actionBar() {
    return Container(
      padding: EdgeInsets.fromLTRB(
          16, 10, 16, 10 + MediaQuery.of(context).padding.bottom),
      decoration: const BoxDecoration(
        border: Border(top: BorderSide(color: Palette.cardBorder)),
      ),
      child: Row(
        children: [
          Expanded(
            child: OutlinedButton.icon(
              onPressed: _busy ? null : _syncTime,
              icon: const Icon(Icons.access_time),
              label: const Text('Время'),
            ),
          ),
          const SizedBox(width: 12),
          Expanded(
            flex: 2,
            child: FilledButton.icon(
              style: FilledButton.styleFrom(backgroundColor: Palette.violet),
              onPressed: _busy ? null : _writeSchedule,
              icon: _busy
                  ? const SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(
                          strokeWidth: 2, color: Colors.white))
                  : const Icon(Icons.save),
              label: const Text('Сохранить'),
            ),
          ),
        ],
      ),
    );
  }
}
