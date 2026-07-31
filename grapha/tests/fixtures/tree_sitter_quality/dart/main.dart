import 'formatter.dart';

/// Coordinates the Dart fixture.
class DartWorker extends BaseWorker implements Runnable {
  final void Function() onReady = DartWorker.reportReady;
  final String label = formatLabel('dart');

  @override
  void run() {
    onReady();
  }

  static void reportReady() {}

  String _debugLabel() => label;
}

abstract class BaseWorker {}

abstract class Runnable {
  void run();
}

enum DartStatus {
  ready,
  stopped,
}

void main() {
  DartWorker().run();
}
