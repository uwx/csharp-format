class A
{
    void M()
    {
    }

    int F;
    int P { get; set; }

    void N()
    {
    }

    int G;

    class Nested
    {
    }

    int H;

    enum E
    {
        X,
        Y
    }

    int I;

    A()
    {
    }

    ~A()
    {
    }

    public static A operator +(A a, A b)
    {
        return a;
    }
}

interface I
{
    void M();
    int P { get; set; }
}

struct S
{
    int F;

    void M()
    {
    }
}