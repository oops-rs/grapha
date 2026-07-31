from helpers import BaseWorker, format_label


class PythonWorker(BaseWorker):
    """Coordinates the Python fixture."""

    on_ready = staticmethod(lambda: report_ready())
    label = format_label("python")

    def run(self):
        self.on_ready()

    def _debug_label(self):
        return self.label


def report_ready():
    pass


worker = PythonWorker()
worker.run()
