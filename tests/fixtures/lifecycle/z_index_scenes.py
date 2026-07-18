from manim import Circle, Dot, Scene, Square, VGroup


def restyle(mob):
    mob.set_z_index(3)
    restyle(mob)


class ZFacts(Scene):
    def construct(self):
        base = Square()
        kw = Square(z_index=2)
        neg = Circle()
        neg.set_z_index(-1)
        self.add(base, kw, neg)
        self.wait()


class ZFamily(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        group = VGroup(a, b)
        self.add(group)
        group.set_z_index(2)
        self.wait()


class ZUnknown(Scene):
    def construct(self):
        a = Square()
        k = 1
        a.set_z_index(k)
        b = Square(**{"z_index": 2})
        helped = Circle()
        restyle(helped)
        leaf = Dot()
        leaf.set_z_index(4, family=False)
        self.add(a, b, helped, leaf)
        self.wait()
