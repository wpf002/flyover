import React from "react";

export interface Props {
  title: string;
}

export function View(props: Props) {
  return <div className="view">{props.title}</div>;
}
