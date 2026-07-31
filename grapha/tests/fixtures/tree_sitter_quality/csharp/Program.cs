using System;
using Quality.Support;

new Quality.App.CSharpWorker().Run();

namespace Quality.App
{
    /// <summary>Coordinates the C# fixture.</summary>
    public sealed class CSharpWorker : BaseWorker, IWorker
    {
        public Action OnReady = ReportReady;
        private readonly string label = Formatter.Format("csharp");

        public void Run()
        {
            OnReady();
        }

        private static void ReportReady() {}
    }

    public abstract class BaseWorker {}

    public interface IWorker
    {
        void Run();
    }

    public enum CSharpStatus
    {
        Ready,
        Stopped,
    }
}
