using System;

[Serializable]
[Obsolete]
class C
{
    [A] [B] int f;

    [A]
    [B]
    void M(int a)
    {
        if (a > 0)
        {
            M(a);
        }

// column zero comment
        int[] items = [1, 2,];
        M(a, items);
        // trailing comment
    }

    [A] [B] int P { get; set; }
}