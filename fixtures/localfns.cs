class A
{
    void M(int[] xs)
    {
        int A1(int z) => z;
        System.Console.WriteLine(A1(1));
        int A2(int z) { return z; }
        System.Console.WriteLine(A2(1));
        [My] int A3(int z) => z;
        System.Console.WriteLine(A3(1));
        [My] int A4(int z) { return z; }
        System.Console.WriteLine(A4(1));
    }
}
class MyAttribute : System.Attribute { }
