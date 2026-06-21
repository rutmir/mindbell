// Главный экран: дышащие волны, текущее намерение и поиск устройства.

import 'package:flutter/material.dart';
import 'package:flutter_blue_plus/flutter_blue_plus.dart';

import 'ble.dart';
import 'config_page.dart';
import 'intention.dart';
import 'main.dart';
import 'ripple.dart';

class ScanPage extends StatefulWidget {
  const ScanPage({super.key});
  @override
  State<ScanPage> createState() => _ScanPageState();
}

class _ScanPageState extends State<ScanPage> {
  List<ScanResult> _results = [];
  bool _scanning = false;
  String _intention = '';

  @override
  void initState() {
    super.initState();
    FlutterBluePlus.scanResults.listen((r) {
      if (mounted) setState(() => _results = r);
    });
    FlutterBluePlus.isScanning.listen((s) {
      if (mounted) setState(() => _scanning = s);
    });
    IntentionStore.load().then((v) {
      if (mounted) setState(() => _intention = v);
    });
  }

  Future<void> _startScan() async {
    final err = await MindBell.ensureReady();
    if (err != null) {
      _toast(err);
      return;
    }
    setState(() => _results = []);
    try {
      await FlutterBluePlus.startScan(
        timeout: const Duration(seconds: 12),
        withNames: const [MindBell.deviceName],
      );
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

  Future<void> _editIntention() async {
    final ctrl = TextEditingController(text: _intention);
    final result = await showModalBottomSheet<String>(
      context: context,
      isScrollControlled: true,
      backgroundColor: Palette.bgTop,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(top: Radius.circular(24)),
      ),
      builder: (ctx) => Padding(
        padding: EdgeInsets.only(
          left: 20,
          right: 20,
          top: 20,
          bottom: MediaQuery.of(ctx).viewInsets.bottom + 20,
        ),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text('Что держишь в фокусе?',
                style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600)),
            const SizedBox(height: 6),
            const Text(
              'Короткая мысль, к которой устройство будет возвращать внимание.',
              style: TextStyle(color: Palette.textDim, fontSize: 13),
            ),
            const SizedBox(height: 16),
            TextField(
              controller: ctrl,
              autofocus: true,
              maxLength: 80,
              textCapitalization: TextCapitalization.sentences,
              decoration: const InputDecoration(
                hintText: 'например: «здесь и сейчас»',
                border: OutlineInputBorder(),
              ),
              onSubmitted: (v) => Navigator.pop(ctx, v),
            ),
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton(
                onPressed: () => Navigator.pop(ctx, ctrl.text),
                child: const Text('Сохранить'),
              ),
            ),
          ],
        ),
      ),
    );
    if (result != null) {
      await IntentionStore.save(result);
      if (mounted) setState(() => _intention = result.trim());
    }
  }

  @override
  Widget build(BuildContext context) {
    final found = _results.where((r) => _name(r) == MindBell.deviceName).toList();

    return GradientScaffold(
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.fromLTRB(20, 24, 20, 32),
          children: [
            const Center(
              child: BreathingRipple(
                size: 220,
                color: Palette.teal,
                active: true,
                child: Icon(Icons.self_improvement,
                    size: 56, color: Colors.white),
              ),
            ),
            const SizedBox(height: 8),
            const Center(
              child: Text('MindBell',
                  style:
                      TextStyle(fontSize: 28, fontWeight: FontWeight.w600)),
            ),
            const SizedBox(height: 4),
            const Center(
              child: Text('возвращайся к выбранной мысли',
                  style: TextStyle(color: Palette.textDim)),
            ),
            const SizedBox(height: 28),
            _intentionCard(),
            const SizedBox(height: 24),
            _scanButton(),
            const SizedBox(height: 16),
            ...found.map(_deviceTile),
            if (found.isEmpty && !_scanning)
              const Padding(
                padding: EdgeInsets.symmetric(vertical: 16),
                child: Text(
                  'Чтобы устройство откликнулось, нажми на нём кнопку (или RST) — '
                  'оно слушает Bluetooth только ~30 секунд после пробуждения.',
                  style: TextStyle(color: Palette.textDim, fontSize: 13),
                  textAlign: TextAlign.center,
                ),
              ),
          ],
        ),
      ),
    );
  }

  Widget _intentionCard() {
    final has = _intention.isNotEmpty;
    return GlassCard(
      onTap: _editIntention,
      child: Row(
        children: [
          const Icon(Icons.center_focus_strong, color: Palette.teal),
          const SizedBox(width: 14),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(has ? 'В фокусе' : 'Задай намерение',
                    style: const TextStyle(
                        color: Palette.textDim, fontSize: 12)),
                const SizedBox(height: 2),
                Text(
                  has ? _intention : 'нажми, чтобы выбрать мысль',
                  style: TextStyle(
                    fontSize: 17,
                    fontWeight: has ? FontWeight.w600 : FontWeight.w400,
                    color: has ? Colors.white : Palette.textDim,
                  ),
                ),
              ],
            ),
          ),
          const Icon(Icons.edit_outlined, color: Palette.textDim, size: 18),
        ],
      ),
    );
  }

  Widget _scanButton() {
    return SizedBox(
      height: 56,
      child: FilledButton.icon(
        style: FilledButton.styleFrom(
          backgroundColor: Palette.violet,
          shape: RoundedRectangleBorder(
              borderRadius: BorderRadius.circular(18)),
        ),
        onPressed: _scanning ? null : _startScan,
        icon: _scanning
            ? const SizedBox(
                width: 20,
                height: 20,
                child: CircularProgressIndicator(
                    strokeWidth: 2, color: Colors.white),
              )
            : const Icon(Icons.bluetooth_searching),
        label: Text(_scanning ? 'Ищу устройство…' : 'Найти устройство'),
      ),
    );
  }

  Widget _deviceTile(ScanResult r) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: GlassCard(
        onTap: () async {
          await FlutterBluePlus.stopScan();
          if (!mounted) return;
          Navigator.of(context).push(MaterialPageRoute(
            builder: (_) => ConfigPage(device: r.device, intention: _intention),
          ));
        },
        child: Row(
          children: [
            const Icon(Icons.notifications_active, color: Palette.teal),
            const SizedBox(width: 14),
            Expanded(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Text(_name(r),
                      style: const TextStyle(
                          fontSize: 16, fontWeight: FontWeight.w600)),
                  Text('сигнал ${r.rssi} dBm',
                      style: const TextStyle(
                          color: Palette.textDim, fontSize: 12)),
                ],
              ),
            ),
            const Icon(Icons.chevron_right, color: Palette.textDim),
          ],
        ),
      ),
    );
  }
}
