package quality.javafixture;

import quality.javafixture.support.Formatter;

/** Coordinates the Java fixture. */
public final class Main extends BaseWorker implements Runnable {
    private final Runnable onReady = Main::reportReady;
    private final String label = Formatter.format("java");

    @Override
    public void run() {
        onReady.run();
    }

    public static void main(String[] args) {
        new Main().run();
    }

    private static void reportReady() {}
}

class BaseWorker {}

enum JavaStatus {
    READY,
    STOPPED
}
