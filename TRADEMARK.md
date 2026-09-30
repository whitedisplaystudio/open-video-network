# Trademarks

The licence covers the code. This covers the name.

**Open Video Network**, **ourvideo**, and the project's logos and other visual
marks are trademarks of the project. Nothing in the
[AGPL](LICENSE) grants any right to use them: section 7(e) of that licence
says so explicitly, and this document says what is and is not allowed.

The point is narrow. Anyone may take this code and do anything the licence
permits — that freedom is the licence's job and this document does not touch
it. What this document stops is a copy passing itself off as the original.

## You do not need permission to

* say your software **works with**, **is compatible with**, **is based on**, or
  **is a fork of** Open Video Network, as a plain statement of fact;
* use the name to refer to this project — in an article, a comparison, a talk,
  a bug report, a package description;
* keep the **protocol identifiers** exactly as they are. Strings like
  `/ovn/kad/1.0.0`, `/ovn/chunk/1.0.0`, the gossip topic names, the
  `ourvideo://` link scheme and the `ovn` multicodec usage are technical
  identifiers, not branding. A fork **must** keep them unchanged or it will not
  interoperate, and interoperating is the whole point;
* redistribute unmodified builds of this project under its own name — a distro
  package, a mirror, a copy on a USB stick.

## You do need permission to

* name your **modified** version Open Video Network or ourvideo, or anything
  close enough to be mistaken for them;
* use the logos or marks as the identity of your own product;
* imply that the project endorses, reviewed, or is responsible for your
  version;
* register a domain, an app-store listing, a package name or a social account
  that reads as the official one.

## If you fork and modify, rename

Pick your own name and your own binary name. Then say what it is:

> Nebulacast — a video player built on Open Video Network. Not affiliated with
> the Open Video Network project.

That sentence is fine and needs no permission. What is not fine is shipping
modified software called `ourvideo`, because then a bug in your version is a
bug in our reputation, and a user who was handed your build cannot tell.

## Why this exists at all

The binary is unsigned and distributed without a publisher anyone can check.
The only thing a user has to go on is the name and the repository it came from.
If a modified build can wear the same name, that last piece of ground goes
too — and for software that holds a person's viewing history on their own
machine, it matters who they are actually trusting.

## Asking

Open an issue. Permission for a specific use is usually not a problem; being
unable to tell the difference afterwards is.
