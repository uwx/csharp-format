using System;
using System.Linq;

class A
{
    void M(int[] xs)
    {
        Func<int, int> f = x => x + 1;
        Func<int, int> g = (x) => { return x + 1; };
        Func<int, int, int> h = (x, y) => x + y;
        Action a = delegate { };
        Action<int> b = delegate(int x) { };
        var q = from x in xs where x > 0 select x * 2;

        int Local(int z)
        {
            return z + 1;
        }

        int LocalExpr(int z) => z - 1;
        Console.WriteLine($"{f(1)} {g(2)} {h(1, 2)} {q.Count()} {Local(1)} {LocalExpr(2)}");
    }
}