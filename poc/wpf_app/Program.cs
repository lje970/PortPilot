// POC-D:C#/WPF(.NET 8)+ System.IO.Ports 最小串口收流工程(无 XAML,纯代码构建 UI)。
// 后台线程阻塞读 → ConcurrentQueue;DispatcherTimer 33ms 节流上屏。
// 遵循 poc/probe_protocol.md v1。

using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.Ports;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Threading;

public static class Program
{
    public class Args
    {
        public string Port = "COM1";
        public int Baud = 921600;
        public double Seconds = 30;
        public string Out = "probe_results.jsonl";
    }

    [STAThread]
    public static int Main(string[] argv)
    {
        var a = new Args();
        for (int i = 0; i < argv.Length - 1; i++)
        {
            switch (argv[i])
            {
                case "--port": a.Port = argv[i + 1]; break;
                case "--baud": a.Baud = int.Parse(argv[i + 1]); break;
                case "--seconds": a.Seconds = double.Parse(argv[i + 1]); break;
                case "--out": a.Out = argv[i + 1]; break;
            }
        }

        var app = new Application();
        var win = new PocWindow(a);
        int code = 0;
        app.Run(win);
        return code;
    }
}

public class PocWindow : Window
{
    private readonly Program.Args a;
    private readonly TextBlock counter = new() { FontSize = 28, FontWeight = FontWeights.Bold, Margin = new Thickness(8) };
    private readonly TextBox log = new()
    {
        IsReadOnly = true, FontFamily = new FontFamily("Consolas"), FontSize = 11,
        Height = 320, Background = Brushes.Black, Foreground = Brushes.LightGray,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
    };

    private readonly ConcurrentQueue<(byte[] data, int n, long arrival)> queue = new();
    private readonly ConcurrentQueue<double> iarSamples = new();
    private long lastReadTs = -1;
    private SerialPort port;
    private volatile bool quitFlag;
    private int reconnects;
    private int initFails;
    private long startTs;
    private long rxBytes;
    private readonly List<double> latSamples = new();
    private int rendersThisSec;
    private readonly Stopwatch clock = Stopwatch.StartNew();
    private StreamWriter probe;
    private int sec;

    public PocWindow(Program.Args args)
    {
        a = args;
        Title = "POC-D WPF";
        Width = 560; Height = 480;
        var panel = new StackPanel();
        panel.Children.Add(counter);
        panel.Children.Add(log);
        Content = panel;

        probe = new StreamWriter(a.Out, append: true) { AutoFlush = true };
        startTs = Stopwatch.GetTimestamp();
        Task.Run(SerialLoop);

        var uiTimer = new DispatcherTimer(DispatcherPriority.Render)
        { Interval = TimeSpan.FromMilliseconds(33) };
        uiTimer.Tick += (_, _) => Flush();
        uiTimer.Start();

        var secTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        secTimer.Tick += (_, _) => ReportSecond();
        secTimer.Start();

        var exitTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(a.Seconds + 5) };
        exitTimer.Tick += (_, _) => { quitFlag = true; probe.Close(); Application.Current.Shutdown(); };
        exitTimer.Start();
    }

    private void OpenPort()
    {
        port = new SerialPort(a.Port, a.Baud)
        { ReadTimeout = 200, WriteTimeout = 50, ReadBufferSize = 1 << 18 };  // 256KB 驱动缓冲,抗调度/GC 停顿
        port.Open();
        port.DiscardInBuffer();
        Console.WriteLine($"[wpf] opened {a.Port} @ {a.Baud}");
    }

    private void SerialLoop()
    {
        var buf = new byte[65536];
        bool first = true;
        while (!quitFlag)
        {
            try
            {
                if (first) { OpenPort(); first = false; }
                int n = port.Read(buf, 0, buf.Length);
                if (n > 0)
                {
                    long now = Stopwatch.GetTimestamp();
                    if (lastReadTs > 0) iarSamples.Enqueue((now - lastReadTs) * 1000.0 / Stopwatch.Frequency);
                    lastReadTs = now;
                    var copy = System.Buffers.ArrayPool<byte>.Shared.Rent(n);  // 复用缓冲,避免每次 Read 分配触发 GC
                    Buffer.BlockCopy(buf, 0, copy, 0, n);
                    queue.Enqueue((copy, n, now));
                }
            }
            catch (TimeoutException) { continue; }  // 读超时是常态轮询,不断线
            catch (Exception ex) when (ex is IOException || ex is UnauthorizedAccessException
                                                || ex is OperationCanceledException)
            {
                if (quitFlag) break;
                if (first && ++initFails >= 5)
                {
                    Console.Error.WriteLine($"[wpf] 无法打开 {a.Port}:{ex.Message}");
                    Environment.Exit(2);
                }
                Console.WriteLine($"[wpf] serial error: {ex.Message} → reconnect in 500ms");
                reconnects++;
                try { port?.Close(); } catch { }
                Thread.Sleep(500);
                first = true;
            }
        }
        try { port?.Close(); } catch { }
    }

    private void Flush()   // UI 节流帧:33ms 一帧
    {
        int rendered = 0;
        double newestArrivalMs = 0;
        while (queue.TryDequeue(out var item))
        {
            rxBytes += item.n;
            double arrMs = (item.arrival - startTs) * 1000.0 / Stopwatch.Frequency;
            newestArrivalMs = Math.Max(newestArrivalMs, arrMs);
            System.Buffers.ArrayPool<byte>.Shared.Return(item.data);
            rendered++;
        }
        if (rendered > 0)
        {
            rendersThisSec++;
            double displayMs = (Stopwatch.GetTimestamp() - startTs) * 1000.0 / Stopwatch.Frequency;
            latSamples.Add(displayMs - newestArrivalMs);
            counter.Text = $"{Interlocked.Read(ref rxBytes)} B";
            // 滚动预览省略:WPF 帧内频繁拼字符串同样计入 UI 开销,保留计数即可
        }
    }

    private void ReportSecond()
    {
        sec++;
        latSamples.Sort();
        double p95 = latSamples.Count > 0 ? latSamples[Math.Max(0, (int)(latSamples.Count * 0.95) - 1)] : 0;
        var iar = new List<double>();
        while (iarSamples.TryDequeue(out double v)) iar.Add(v);
        iar.Sort();
        double iarP95 = iar.Count > 0 ? iar[Math.Max(0, (int)(iar.Count * 0.95) - 1)] : 0;
        probe.WriteLine(System.Text.Json.JsonSerializer.Serialize(new
        {
            t = sec,
            rx_bytes = Interlocked.Read(ref rxBytes),
            ui_fps = (double)rendersThisSec,
            latency_p95_ms = Math.Round(p95, 2),
            inter_arrival_p95_ms = Math.Round(iarP95, 2),
            reconnects
        }));
        Title = $"POC-D WPF · {sec}s · {rendersThisSec} fps";
        latSamples.Clear();
        rendersThisSec = 0;
    }
}
