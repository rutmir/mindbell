// Дышащие волны: расходящиеся кольца вокруг точки-фокуса. Метафора устройства —
// внимание раз за разом возвращается к выбранной мысли.

import 'dart:math' as math;
import 'package:flutter/material.dart';

class BreathingRipple extends StatefulWidget {
  final double size;
  final Color color;
  final Widget? child;
  final bool active;

  const BreathingRipple({
    super.key,
    this.size = 220,
    required this.color,
    this.child,
    this.active = true,
  });

  @override
  State<BreathingRipple> createState() => _BreathingRippleState();
}

class _BreathingRippleState extends State<BreathingRipple>
    with SingleTickerProviderStateMixin {
  late final AnimationController _c =
      AnimationController(vsync: this, duration: const Duration(seconds: 7))
        ..repeat();

  @override
  void didUpdateWidget(BreathingRipple old) {
    super.didUpdateWidget(old);
    if (widget.active && !_c.isAnimating) {
      _c.repeat();
    } else if (!widget.active && _c.isAnimating) {
      _c.stop();
    }
  }

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return SizedBox(
      width: widget.size,
      height: widget.size,
      child: Stack(
        alignment: Alignment.center,
        children: [
          AnimatedBuilder(
            animation: _c,
            builder: (_, __) => CustomPaint(
              size: Size.square(widget.size),
              painter: _RipplePainter(_c.value, widget.color),
            ),
          ),
          if (widget.child != null) widget.child!,
        ],
      ),
    );
  }
}

class _RipplePainter extends CustomPainter {
  final double t; // 0..1, фаза анимации
  final Color color;
  static const int rings = 4;

  _RipplePainter(this.t, this.color);

  @override
  void paint(Canvas canvas, Size size) {
    final center = size.center(Offset.zero);
    final maxR = size.width / 2;

    for (int i = 0; i < rings; i++) {
      final phase = (t + i / rings) % 1.0;
      final r = maxR * (0.18 + 0.82 * phase);
      // Появляется мягко, к краю растворяется.
      final fade = math.sin(phase * math.pi);
      final paint = Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 2.2
        ..color = color.withValues(alpha: 0.38 * fade);
      canvas.drawCircle(center, r, paint);
    }

    // Точка-фокус с лёгким «дыханием».
    final pulse = 0.5 + 0.5 * math.sin(t * 2 * math.pi);
    final dotR = maxR * (0.085 + 0.02 * pulse);
    canvas.drawCircle(
      center,
      dotR + 4,
      Paint()..color = color.withValues(alpha: 0.18),
    );
    canvas.drawCircle(center, dotR, Paint()..color = color);
  }

  @override
  bool shouldRepaint(_RipplePainter old) => old.t != t || old.color != color;
}
