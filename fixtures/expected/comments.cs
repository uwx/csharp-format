#region Header

// file header
using System;

#endregion

/// <summary>Docs.</summary>
class A
{
    // field comment
    int f; // trailing

    /* block */
    void M( /* param */ int x)
    {
        // before statement
        int y = x; // after statement
        /* inline */
        y++;
    }
}
#pragma warning disable CS0169
#pragma warning restore CS0169
class D
{
}