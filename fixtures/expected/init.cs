class A
{
    void M()
    {
        var o = new Foo { A = 1, B = 2 };
        var a = new[] { 1, 2, 3 };
        if (o != null)
        {
        }

        if (o != null)
        {
            M();
        }

        System.Func<int, int> f = x => { return x + 1; };
        var t = (1, 2);
    }
}