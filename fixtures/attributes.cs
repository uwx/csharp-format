using System;

[assembly: CLSCompliant(true)]

namespace N
{
    [Serializable]
    public class A
    {
        [Obsolete("x")]
        public void M([NotNull] string s)
        {
            [My] int Local([My] int q) => q;
            _ = Local(1);
        }
    }

    public class MyAttribute : Attribute { }
    public class NotNullAttribute : Attribute { }
}
