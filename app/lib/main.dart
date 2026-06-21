// MindBell — спутник практики внимательности.
// Устройство раз за разом мягко возвращает внимание к выбранной мысли;
// это приложение настраивает, когда и как часто оно это делает.

import 'package:flutter/material.dart';
import 'scan_page.dart';

void main() => runApp(const MindBellApp());

/// Спокойная «медитативная» палитра.
class Palette {
  static const bgTop = Color(0xFF1B1D33);
  static const bgBottom = Color(0xFF0D1320);
  static const violet = Color(0xFF8B7CFF);
  static const teal = Color(0xFF4FD1C5);
  static const card = Color(0x12FFFFFF); // белый ~7%
  static const cardBorder = Color(0x1FFFFFFF);
  static const textDim = Color(0xFF9AA0B4);
}

class MindBellApp extends StatelessWidget {
  const MindBellApp({super.key});

  @override
  Widget build(BuildContext context) {
    final scheme = ColorScheme.fromSeed(
      seedColor: Palette.violet,
      brightness: Brightness.dark,
    ).copyWith(secondary: Palette.teal);

    return MaterialApp(
      title: 'MindBell',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        useMaterial3: true,
        colorScheme: scheme,
        scaffoldBackgroundColor: Palette.bgBottom,
        textTheme: Typography.whiteMountainView.apply(
          bodyColor: Colors.white,
          displayColor: Colors.white,
        ),
        snackBarTheme: const SnackBarThemeData(
          behavior: SnackBarBehavior.floating,
        ),
      ),
      home: const ScanPage(),
    );
  }
}

/// Каркас с мягким вертикальным градиентом — общий фон всех экранов.
class GradientScaffold extends StatelessWidget {
  final Widget body;
  final PreferredSizeWidget? appBar;
  final Widget? bottomBar;

  const GradientScaffold({
    super.key,
    required this.body,
    this.appBar,
    this.bottomBar,
  });

  @override
  Widget build(BuildContext context) {
    return Container(
      decoration: const BoxDecoration(
        gradient: LinearGradient(
          begin: Alignment.topCenter,
          end: Alignment.bottomCenter,
          colors: [Palette.bgTop, Palette.bgBottom],
        ),
      ),
      child: Scaffold(
        backgroundColor: Colors.transparent,
        appBar: appBar,
        body: body,
        bottomNavigationBar: bottomBar,
      ),
    );
  }
}

/// Мягкая карточка-контейнер в общем стиле.
class GlassCard extends StatelessWidget {
  final Widget child;
  final EdgeInsetsGeometry padding;
  final VoidCallback? onTap;

  const GlassCard({
    super.key,
    required this.child,
    this.padding = const EdgeInsets.all(16),
    this.onTap,
  });

  @override
  Widget build(BuildContext context) {
    return Material(
      color: Palette.card,
      borderRadius: BorderRadius.circular(20),
      child: InkWell(
        borderRadius: BorderRadius.circular(20),
        onTap: onTap,
        child: Container(
          padding: padding,
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(20),
            border: Border.all(color: Palette.cardBorder),
          ),
          child: child,
        ),
      ),
    );
  }
}
