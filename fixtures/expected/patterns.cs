class A
{
    int M(object o)
    {
        var r = o switch { int i => i, string s => s.Length, _ => 0 };
        if (o is int { } n)
        {
            return n;
        }

        if (o is not null and not string)
        {
            return 1;
        }

        var (a, b) = (1, 2);
        int[] arr = [1, 2, 3];
        var list = new List<int> { 1, 2 };
        return o switch
        {
            0 => 0,
            _ => 1,
        };
    }
}