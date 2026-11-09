class A
{
    void M(int[] xs)
    {
        try { M(xs); } catch (System.Exception e) { System.Console.WriteLine(e); } finally { xs = null; }
        using (var r = new System.IO.StringReader("")) { r.Read(); }
        lock (this) { xs = null; }
        do { xs = null; } while (xs != null);
        foreach (var x in xs) { System.Console.WriteLine(x); }
        checked { xs = null; }
        unchecked { xs = null; }
        switch (xs.Length)
        {
            case 0: break;
            case 1: case 2: xs = null; break;
            default: goto done;
        }
        done: xs = null;
    }
}
