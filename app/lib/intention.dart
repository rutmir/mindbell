// Локальное хранилище «намерения» — той мысли, к которой устройство возвращает
// внимание. Живёт только в телефоне (у устройства нет экрана), но задаёт смысл
// всему расписанию: каждый вибросигнал — это возврат к этой идее.

import 'package:shared_preferences/shared_preferences.dart';

class IntentionStore {
  static const _key = 'intention';

  static Future<String> load() async {
    final p = await SharedPreferences.getInstance();
    return p.getString(_key) ?? '';
  }

  static Future<void> save(String value) async {
    final p = await SharedPreferences.getInstance();
    await p.setString(_key, value.trim());
  }
}
